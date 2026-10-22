//! Native history storage. Record encoding and searches are shared with POSIX Fish;
//! synchronization and session ownership are specific to the Windows frontend.

use crate::history_core::{HistoryItem, PersistenceMode, Timestamps, time_to_seconds, yaml};
use fish_widestring::WString;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_ENTRIES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Serializable state follows a native session through command workers. Files and
/// locks are opened only for an individual operation and never serialized.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct History {
    path: Option<PathBuf>,
    loaded: bool,
    entries: Vec<Entry>,
    pending: Vec<Entry>,
    session: String,
    /// Currently executing submission: present for deletion and ordering, but
    /// hidden from search and excluded from writes until evaluation finishes.
    current: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Entry {
    command: String,
    last_added: i64,
    first_added: i64,
    required_paths: Vec<String>,
    /// An optional extension to Fish's YAML records. Keeping each contribution's
    /// identity lets clear-session preserve identical commands from other shells.
    id: Option<String>,
}

impl Entry {
    fn from_item(item: HistoryItem, id: Option<String>) -> Self {
        Self {
            command: item.str().to_string(),
            last_added: time_to_seconds(item.last_added_timestamp()),
            first_added: time_to_seconds(item.first_added_timestamp()),
            required_paths: item
                .get_required_paths()
                .iter()
                .map(ToString::to_string)
                .collect(),
            id,
        }
    }

    fn item(&self, persistence: PersistenceMode) -> HistoryItem {
        let mut item = HistoryItem::new(
            WString::from(self.command.as_str()),
            Timestamps {
                last_added: timestamp(self.last_added),
                first_added: timestamp(self.first_added),
            },
            persistence,
        );
        item.set_required_paths(
            self.required_paths
                .iter()
                .map(|path| WString::from(path.as_str()))
                .collect(),
        );
        item
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new(None)
    }
}

impl History {
    /// `None` is private or disabled history: all operations remain in memory.
    /// A `*.yaml` path can import an existing Rustyline file at the same path
    /// without that suffix, once, while leaving the legacy file untouched.
    pub(super) fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            loaded: false,
            entries: Vec::new(),
            pending: Vec::new(),
            session: unique_id(),
            current: None,
        }
    }

    pub(super) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Return distinct commands, newest first, with Fish's shared timestamps.
    pub(super) fn items(&mut self) -> io::Result<Vec<HistoryItem>> {
        self.load()?;
        let persistence = if self.path.is_some() {
            PersistenceMode::Disk
        } else {
            PersistenceMode::Memory
        };
        let mut positions = HashMap::<&str, usize>::new();
        let mut items: Vec<(usize, HistoryItem)> = Vec::new();
        for (position, entry) in self.entries.iter().enumerate() {
            if is_current(entry, self.current.as_deref()) {
                continue;
            }
            if let Some(&index) = positions.get(entry.command.as_str()) {
                // All entries in this store use the same persistence mode.
                items[index]
                    .1
                    .merge(entry.item(persistence))
                    .expect("same history command");
                items[index].0 = position;
            } else {
                positions.insert(&entry.command, items.len());
                items.push((position, entry.item(persistence)));
            }
        }
        items.sort_unstable_by_key(|(position, _)| std::cmp::Reverse(*position));
        Ok(items.into_iter().map(|(_, item)| item).collect())
    }

    /// Editor history is oldest first and contains the original input spelling.
    pub(super) fn snapshot(&mut self) -> io::Result<Vec<String>> {
        Ok(self
            .items()?
            .into_iter()
            .rev()
            .map(|item| item.str().to_string())
            .collect())
    }

    /// Explicit `history append` accepts leading whitespace and duplicates.
    /// Saving is separate so several appends can share one transaction.
    pub(super) fn append(&mut self, source: &str) -> io::Result<bool> {
        self.load()?;
        if source.is_empty() {
            return Ok(false);
        }
        let now = time_to_seconds(SystemTime::now());
        let entry = Entry {
            command: source.to_owned(),
            last_added: now,
            first_added: now,
            required_paths: Vec::new(),
            id: Some(format!("{}:{}", self.session, unique_id())),
        };
        self.entries.push(entry.clone());
        self.pending.push(entry);
        bound(&mut self.entries);
        bound(&mut self.pending);
        Ok(true)
    }

    /// Reserve a submission before evaluating it. It remains hidden from
    /// searches, but history mutations during evaluation can remove it.
    pub(super) fn begin_command(&mut self, source: &str) -> io::Result<bool> {
        let source = source.trim_end_matches(['\r', '\n']);
        self.load()?;
        if self.current.is_some() {
            return Err(io::Error::other("a history command is already pending"));
        }
        if source.is_empty()
            || source.chars().next().is_some_and(char::is_whitespace)
            || self
                .entries
                .last()
                .is_some_and(|entry| entry.command == source)
        {
            return Ok(false);
        }
        self.append(source)?;
        self.current = self.entries.last().and_then(|entry| entry.id.clone());
        Ok(true)
    }

    /// Reveal and save the completed submission if it has not been deleted.
    pub(super) fn finish_command(&mut self) -> io::Result<()> {
        self.current = None;
        self.save()
    }

    #[cfg(test)]
    fn add_command(&mut self, source: &str) -> io::Result<bool> {
        let added = self.begin_command(source)?;
        self.finish_command()?;
        Ok(added)
    }

    /// Persist only unsaved contributions. A stale session never writes its
    /// loaded snapshot back over deletions or other sessions' newer commands.
    pub(super) fn save(&mut self) -> io::Result<()> {
        self.update(Deletion::None, false).map(|_| ())
    }

    /// Import other sessions' persisted commands into the visible snapshot.
    pub(super) fn merge(&mut self) -> io::Result<()> {
        self.update(Deletion::None, true)?;
        // Like POSIX Fish, merging makes everything seen so far the baseline
        // for clear-session. A pending merge submission keeps its original ID
        // and therefore also belongs to that baseline once it is revealed.
        self.session = unique_id();
        Ok(())
    }

    /// Remove selected complete commands from all sessions, atomically.
    pub(super) fn delete(&mut self, commands: &[String]) -> io::Result<usize> {
        self.update(
            Deletion::Commands(commands.iter().map(String::as_str).collect()),
            false,
        )
    }

    pub(super) fn clear(&mut self) -> io::Result<()> {
        self.update(Deletion::All, false).map(|_| ())
    }

    pub(super) fn clear_session(&mut self) -> io::Result<()> {
        let session = format!("{}:", self.session);
        self.update(Deletion::Session(&session), false).map(|_| ())
    }

    fn load(&mut self) -> io::Result<()> {
        if self.loaded {
            return Ok(());
        }
        if let Some(path) = &self.path {
            self.entries = locked(path, || read_or_import(path))?;
        }
        self.loaded = true;
        Ok(())
    }

    fn update(&mut self, deletion: Deletion<'_>, refresh: bool) -> io::Result<usize> {
        self.load()?;
        let mut deleted = HashSet::new();
        let current = self
            .entries
            .iter()
            .find(|entry| is_current(entry, self.current.as_deref()))
            .cloned();
        if let Some(path) = &self.path {
            if self.pending.is_empty() && matches!(deletion, Deletion::None) && !refresh {
                return Ok(0);
            }
            let entries = locked(path, || {
                let mut disk = read_or_import(path)?;
                // Workers can inherit already-pending entries; identity makes a
                // repeated save of that state idempotent.
                let ids: HashSet<_> = disk
                    .iter()
                    .filter_map(|entry| entry.id.as_deref())
                    .collect();
                let additions: Vec<_> = self
                    .pending
                    .iter()
                    .filter(|entry| {
                        !is_current(entry, self.current.as_deref())
                            && entry.id.as_deref().is_none_or(|id| !ids.contains(id))
                    })
                    .cloned()
                    .collect();
                let has_additions = !additions.is_empty();
                disk.extend(additions);
                remove(&mut disk, &deletion, &mut deleted);
                order(&mut disk);
                bound(&mut disk);
                if has_additions || !deleted.is_empty() || matches!(deletion, Deletion::All) {
                    rewrite(path, &disk)?;
                }
                Ok(disk)
            })?;
            if refresh {
                self.entries = entries;
                if let Some(current) = current {
                    self.entries.push(current);
                    order(&mut self.entries);
                    bound(&mut self.entries);
                }
            }
        }
        remove(&mut self.entries, &deletion, &mut deleted);
        remove(&mut self.pending, &deletion, &mut deleted);
        self.pending
            .retain(|entry| is_current(entry, self.current.as_deref()));
        if !self
            .entries
            .iter()
            .any(|entry| is_current(entry, self.current.as_deref()))
        {
            self.current = None;
        }
        Ok(deleted.len())
    }
}

fn is_current(entry: &Entry, current: Option<&str>) -> bool {
    current.is_some_and(|current| entry.id.as_deref() == Some(current))
}

fn order(entries: &mut [Entry]) {
    // Native IDs carry creation time at nanosecond precision. This preserves
    // pending-command order even when a nested `history append` saves first.
    // Imported entries retain their stable order within Fish's timestamps.
    entries.sort_by_key(|entry| {
        let creation = entry.id.as_deref().and_then(|id| {
            let (_, contribution) = id.split_once(':')?;
            let nanos = contribution.split('-').nth(1)?;
            u128::from_str_radix(nanos, 16).ok()
        });
        (entry.last_added, creation)
    });
}

enum Deletion<'a> {
    None,
    All,
    Session(&'a str),
    Commands(HashSet<&'a str>),
}

fn remove(entries: &mut Vec<Entry>, deletion: &Deletion<'_>, deleted: &mut HashSet<String>) {
    entries.retain(|entry| {
        let delete = match deletion {
            Deletion::None => false,
            Deletion::All => true,
            Deletion::Session(session) => entry
                .id
                .as_deref()
                .is_some_and(|id| id.starts_with(session)),
            Deletion::Commands(commands) => commands.contains(entry.command.as_str()),
        };
        if delete {
            deleted.insert(entry.command.clone());
        }
        !delete
    });
}

fn timestamp(seconds: i64) -> SystemTime {
    if let Ok(seconds) = u64::try_from(seconds) {
        UNIX_EPOCH + Duration::from_secs(seconds)
    } else {
        UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
    }
}

fn unique_id() -> String {
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn bound(entries: &mut Vec<Entry>) {
    if entries.len() > MAX_ENTRIES {
        entries.drain(..entries.len() - MAX_ENTRIES);
    }
}

use super::storage::locked;

fn read_bytes(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    // Older Rustyline writers lock this file. A shared lock prevents migration
    // from reading an incompletely appended legacy entry.
    let lock = fd_lock::RwLock::new(file);
    let file = lock.read()?;
    (&*file).take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "native history exceeds 64 MiB",
        ));
    }
    Ok(Some(bytes))
}

fn read_or_import(path: &Path) -> io::Result<Vec<Entry>> {
    if let Some(bytes) = read_bytes(path)? {
        return Ok(decode(&bytes));
    }
    if path
        .extension()
        .is_some_and(|extension| extension == "yaml")
    {
        let legacy = path.with_extension("");
        if let Some(bytes) = read_bytes(&legacy)? {
            let entries = decode_legacy(&bytes)?;
            // A separate filename lets older running editors continue to append
            // their own format. The original also remains the migration backup.
            rewrite(path, &entries)?;
            return Ok(entries);
        }
    }
    Ok(Vec::new())
}

fn decode(bytes: &[u8]) -> Vec<Entry> {
    let mut cursor = 0;
    let mut offsets = Vec::new();
    while let Some(offset) = yaml::offset_of_next_item_fish_2_0(bytes, &mut cursor, None) {
        offsets.push(offset);
    }
    offsets.push(bytes.len());
    let mut entries = Vec::new();
    for offsets in offsets.windows(2) {
        let bytes = &bytes[offsets[0]..offsets[1]];
        if let Some(item) = yaml::decode_item_fish_2_0(bytes) {
            let id = bytes.split(|byte| *byte == b'\n').find_map(|line| {
                let id = line.strip_prefix(b"  native_id: ")?;
                let id = std::str::from_utf8(id).ok()?;
                (!id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() || matches!(byte, b'-' | b':')))
                .then(|| id.to_owned())
            });
            entries.push(Entry::from_item(item, id));
        }
    }
    bound(&mut entries);
    entries
}

fn decode_legacy(bytes: &[u8]) -> io::Result<Vec<Entry>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut lines = text.lines();
    let Some(version) = lines.next() else {
        return Ok(Vec::new());
    };
    let multiline = version == "#V2";
    let mut entries = Vec::new();
    for line in (!multiline).then_some(version).into_iter().chain(lines) {
        if line.is_empty() {
            continue;
        }
        let command = if multiline && valid_legacy_escapes(line.as_bytes()) {
            String::from_utf8(yaml::unescape_yaml_fish_2_0(line.as_bytes()))
                .expect("unescaping preserves UTF-8")
        } else {
            line.to_owned()
        };
        entries.push(Entry {
            command,
            last_added: 0,
            first_added: 0,
            required_paths: Vec::new(),
            id: None,
        });
    }
    bound(&mut entries);
    Ok(entries)
}

fn valid_legacy_escapes(bytes: &[u8]) -> bool {
    let mut bytes = bytes.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'\\' && !matches!(bytes.next(), Some(b'\\' | b'n')) {
            return false;
        }
    }
    true
}

fn rewrite(path: &Path, entries: &[Entry]) -> io::Result<()> {
    super::storage::rewrite(path, |file| {
        for entry in entries {
            entry.item(PersistenceMode::Disk).write_to(file)?;
            if let Some(id) = &entry.id {
                writeln!(file, "  native_id: {id}")?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("fish-native-history-{}", unique_id()));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
        fn path(&self) -> PathBuf {
            self.0.join("native_fish_history.yaml")
        }
        fn history(&self) -> History {
            History::new(Some(self.path()))
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn interleaved_saves_deletions_and_merges_preserve_other_sessions() {
        let directory = Directory::new();
        let mut first = directory.history();
        let mut second = directory.history();
        assert!(first.snapshot().unwrap().is_empty());
        assert!(second.snapshot().unwrap().is_empty());
        first.append("echo first").unwrap();
        first.save().unwrap();
        second.append("echo second").unwrap();
        second.save().unwrap();
        first.delete(&["echo first".to_owned()]).unwrap();
        second.append("echo third").unwrap();
        second.save().unwrap();
        first.merge().unwrap();
        assert_eq!(first.snapshot().unwrap(), ["echo second", "echo third"]);
        assert_eq!(
            directory.history().snapshot().unwrap(),
            ["echo second", "echo third"]
        );
    }

    #[test]
    fn clear_session_preserves_identical_commands_from_other_sessions() {
        let directory = Directory::new();
        let mut earlier = directory.history();
        earlier.append("shared").unwrap();
        earlier.save().unwrap();
        let mut first = directory.history();
        let mut second = directory.history();
        first.append("shared").unwrap();
        first.append("only first").unwrap();
        first.save().unwrap();
        second.append("shared").unwrap();
        second.append("only second").unwrap();
        second.save().unwrap();
        first.clear_session().unwrap();
        let actual = directory.history().snapshot().unwrap();
        assert_eq!(actual, ["shared", "only second"]);
        second.clear_session().unwrap();
        assert_eq!(directory.history().snapshot().unwrap(), ["shared"]);
    }

    #[test]
    fn merge_resets_the_clear_session_boundary() {
        let directory = Directory::new();
        let mut history = directory.history();
        history.append("before merge").unwrap();
        history.merge().unwrap();
        history.append("after merge").unwrap();
        history.save().unwrap();
        history.clear_session().unwrap();
        assert_eq!(history.snapshot().unwrap(), ["before merge"]);
        assert_eq!(directory.history().snapshot().unwrap(), ["before merge"]);
    }

    #[test]
    fn pending_merge_submission_becomes_part_of_the_clear_session_baseline() {
        let directory = Directory::new();
        let mut history = directory.history();
        history.append("before merge").unwrap();
        history.begin_command("history merge").unwrap();
        history.merge().unwrap();
        history.append("after merge").unwrap();
        history.clear_session().unwrap();
        assert_eq!(history.snapshot().unwrap(), ["before merge"]);
        history.finish_command().unwrap();
        assert_eq!(
            history.snapshot().unwrap(),
            ["before merge", "history merge"]
        );
        assert_eq!(
            directory.history().snapshot().unwrap(),
            ["before merge", "history merge"]
        );
    }

    #[test]
    fn clearing_does_not_let_stale_sessions_resurrect_old_entries() {
        let directory = Directory::new();
        let mut first = directory.history();
        first.append("old").unwrap();
        first.save().unwrap();
        let mut stale = directory.history();
        assert_eq!(stale.snapshot().unwrap(), ["old"]);
        first.clear().unwrap();
        stale.append("new").unwrap();
        stale.save().unwrap();
        assert_eq!(directory.history().snapshot().unwrap(), ["new"]);
    }

    #[test]
    fn legacy_migration_preserves_original_and_imports_multiline_once() {
        let directory = Directory::new();
        let legacy = directory.path().with_extension("");
        let bytes = b"#V2\necho one\nbegin\\necho \\nend\necho \\\\path\n";
        fs::write(&legacy, bytes).unwrap();
        let mut history = directory.history();
        assert_eq!(
            history.snapshot().unwrap(),
            ["echo one", "begin\necho \nend", "echo \\path"]
        );
        assert_eq!(fs::read(&legacy).unwrap(), bytes);
        assert!(
            history
                .items()
                .unwrap()
                .iter()
                .all(|item| item.last_added_timestamp() == UNIX_EPOCH)
        );
        fs::write(&legacy, b"#V2\nadded by old shell\n").unwrap();
        history.merge().unwrap();
        assert!(
            !history
                .snapshot()
                .unwrap()
                .iter()
                .any(|entry| entry == "added by old shell")
        );
        assert!(
            fs::read_to_string(directory.path())
                .unwrap()
                .starts_with("- cmd: ")
        );
    }

    #[test]
    fn shared_codec_retains_unicode_multiline_timestamps_and_paths() {
        let entry = Entry {
            command: "begin\necho 🐟 C:\\path\nend".to_owned(),
            first_added: 123,
            last_added: 456,
            required_paths: vec!["C:\\a path\\🐟".to_owned()],
            id: Some("a-b:c-d".to_owned()),
        };
        let directory = Directory::new();
        rewrite(&directory.path(), std::slice::from_ref(&entry)).unwrap();
        let actual = decode(&fs::read(directory.path()).unwrap());
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].command, entry.command);
        assert_eq!(actual[0].first_added, 123);
        assert_eq!(actual[0].last_added, 456);
        assert_eq!(actual[0].required_paths, entry.required_paths);
        assert_eq!(actual[0].id, entry.id);
    }

    #[test]
    fn worker_snapshots_save_pending_entries_only_once() {
        let directory = Directory::new();
        let mut first = directory.history();
        first.append("one").unwrap();
        let bytes = bincode::serialize(&first).unwrap();
        let mut worker: History = bincode::deserialize(&bytes).unwrap();
        first.save().unwrap();
        worker.save().unwrap();
        assert_eq!(decode(&fs::read(directory.path()).unwrap()).len(), 1);
    }

    #[test]
    fn pending_submission_is_hidden_and_saves_before_appends_from_its_evaluation() {
        let directory = Directory::new();
        let mut history = directory.history();
        history.begin_command("history append after").unwrap();
        assert!(history.snapshot().unwrap().is_empty());
        history.save().unwrap();
        assert!(directory.history().snapshot().unwrap().is_empty());
        history.append("after").unwrap();
        history.save().unwrap();
        assert_eq!(history.snapshot().unwrap(), ["after"]);
        assert_eq!(directory.history().snapshot().unwrap(), ["after"]);
        history.merge().unwrap();
        assert_eq!(history.snapshot().unwrap(), ["after"]);
        history.finish_command().unwrap();
        assert_eq!(
            history.snapshot().unwrap(),
            ["history append after", "after"]
        );
        assert_eq!(
            directory.history().snapshot().unwrap(),
            ["history append after", "after"]
        );
    }

    #[test]
    fn deletion_and_clearing_do_not_resurrect_pending_submissions() {
        let directory = Directory::new();
        let mut history = directory.history();
        history.begin_command("delete me").unwrap();
        assert_eq!(history.delete(&["delete me".to_owned()]).unwrap(), 1);
        history.finish_command().unwrap();
        assert!(history.snapshot().unwrap().is_empty());
        history.begin_command("clear me").unwrap();
        history.clear().unwrap();
        history.finish_command().unwrap();
        assert!(directory.history().snapshot().unwrap().is_empty());
        history.begin_command("clear this session").unwrap();
        history.clear_session().unwrap();
        history.finish_command().unwrap();
        assert!(directory.history().snapshot().unwrap().is_empty());
    }

    #[test]
    fn worker_snapshots_preserve_pending_visibility() {
        let directory = Directory::new();
        let mut parent = directory.history();
        parent.begin_command("current source").unwrap();
        let mut worker: History =
            bincode::deserialize(&bincode::serialize(&parent).unwrap()).unwrap();
        assert!(worker.snapshot().unwrap().is_empty());
        worker.save().unwrap();
        assert!(directory.history().snapshot().unwrap().is_empty());
        parent.finish_command().unwrap();
        worker.finish_command().unwrap();
        assert_eq!(decode(&fs::read(directory.path()).unwrap()).len(), 1);
        assert_eq!(directory.history().snapshot().unwrap(), ["current source"]);
    }

    #[test]
    fn memory_only_history_and_interactive_filters() {
        let mut history = History::new(None);
        assert!(!history.add_command(" hidden").unwrap());
        assert!(!history.add_command("\talso hidden").unwrap());
        assert!(!history.add_command("").unwrap());
        assert!(history.add_command("visible\r\n").unwrap());
        assert!(!history.add_command("visible").unwrap());
        assert!(history.append(" explicit").unwrap());
        history.save().unwrap();
        history.merge().unwrap();
        assert_eq!(history.snapshot().unwrap(), ["visible", " explicit"]);
        history.append("after merge").unwrap();
        history.clear_session().unwrap();
        assert_eq!(history.snapshot().unwrap(), ["visible", " explicit"]);
    }

    #[test]
    fn concurrent_rewrites_keep_all_writer_contributions() {
        let directory = Directory::new();
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let path = directory.path();
                std::thread::spawn(move || {
                    let mut history = History::new(Some(path));
                    for index in 0..15 {
                        history
                            .append(&format!("worker {worker} command {index}"))
                            .unwrap();
                        history.save().unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(directory.history().snapshot().unwrap().len(), 60);
    }
}
