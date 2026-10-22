//! The classes responsible for autoloading functions and completions.

#[cfg(not(windows))]
use crate::{
    env::Environment,
    flogf,
    io::IoChain,
    parser::Parser,
    wutil::{FileId, file_id_for_path},
};
#[cfg(not(windows))]
use fish_common::{ScopeGuard, escape};
use fish_widestring::{L, WExt as _, WString, wcs2bytes, wstr};
use lru::LruCache;
use rust_embed::RustEmbed;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::time;

#[cfg(windows)]
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct FileId {
    path: std::path::PathBuf,
    size: u64,
    modified: Option<time::SystemTime>,
    created: Option<time::SystemTime>,
}

#[cfg(windows)]
fn file_id_for_path(path: &wstr) -> std::io::Result<FileId> {
    let path = std::path::PathBuf::from(fish_widestring::wcs2osstring(path));
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "autoload source is not a file",
        ));
    }
    Ok(FileId {
        // Unlike the POSIX identity, portable metadata has no device/inode pair.
        // Include the path so changing search directories selects the new file
        // even when its size and timestamps match the previously loaded file.
        path: std::fs::canonicalize(&path).unwrap_or(path),
        size: metadata.len(),
        modified: metadata.modified().ok(),
        created: metadata.created().ok(),
    })
}

/// autoload_t is a class that knows how to autoload .fish files from a list of directories. This
/// is used by autoloading functions and completions. It maintains a file cache, which is
/// responsible for potentially cached accesses of files, and then a list of files that have
/// actually been autoloaded. A client may request a file to autoload given a command name, and may
/// be returned a path which it is expected to source.
/// autoload_t does not have any internal locks; it is the responsibility of the caller to lock
/// it.
#[derive(Clone, Default)]
pub struct Autoload {
    /// The environment variable whose paths we observe.
    env_var_name: &'static wstr,

    /// A map from command to the files we have autoloaded.
    autoloaded_files: HashMap<WString, Option<FileId>>,

    /// The list of commands that we are currently autoloading.
    current_autoloading: HashSet<WString>,

    /// The autoload cache.
    cache: AutoloadFileCache,
}

/// Serializable native worker bookkeeping, excluding transient lookup caches.
/// Original file identities travel with evaluated definitions so deserialization
/// cannot accidentally label a changed file as already loaded.
#[cfg(windows)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct AutoloadState {
    loaded_files: Vec<(String, Option<FileId>)>,
    in_progress: Vec<String>,
}

#[derive(RustEmbed)]
#[folder = "share"]
#[exclude = "__fish_build_paths.fish.in"]
pub struct Asset;

pub fn has_asset(cmd: &str) -> bool {
    Asset::get(cmd).is_some()
}

/// List disk and embedded functions without executing their definitions.
/// POSIX directory entries retain Fish's lossless representation of filename bytes.
pub fn function_names(paths: &[WString], get_hidden: bool) -> HashSet<WString> {
    fn insert(names: &mut HashSet<WString>, file: &wstr, get_hidden: bool) {
        if file.len() <= ".fish".len()
            || !file.ends_with(L!(".fish"))
            || (!get_hidden && file.as_char_slice()[0] == '_')
        {
            return;
        }
        names.insert(file[..file.len() - ".fish".len()].to_owned());
    }

    let mut names = HashSet::new();
    for path in paths {
        #[cfg(not(windows))]
        {
            let Ok(mut directory) = crate::wutil::dir_iter::DirIter::new(path) else {
                continue;
            };
            while let Some(entry) = directory.next() {
                if let Ok(entry) = entry {
                    if !entry.is_dir() {
                        insert(&mut names, &entry.name, get_hidden);
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            let Ok(directory) = std::fs::read_dir(fish_widestring::wcs2osstring(path)) else {
                continue;
            };
            for entry in directory.flatten() {
                if !entry.path().is_dir() {
                    insert(
                        &mut names,
                        &fish_widestring::osstr2wcstring(&entry.file_name()),
                        get_hidden,
                    );
                }
            }
        }
    }
    for asset in Asset::iter() {
        if let Some(file) = asset.strip_prefix("functions/") {
            insert(&mut names, &WString::from(file), get_hidden);
        }
    }
    names
}

#[derive(Clone, Copy, PartialEq)]
enum AssetDir {
    Functions,
    Completions,
}

#[derive(Clone, Debug)]
pub enum AutoloadPath {
    OnDisk(WString),
    Embedded(String),
}

#[derive(Clone, Debug)]
pub enum AutoloadResult {
    Path(AutoloadPath),
    Loaded,
    Pending,
    None,
}

#[cfg(all(test, not(windows)))]
impl AutoloadResult {
    fn is_none(&self) -> bool {
        matches!(self, AutoloadResult::None)
    }
    fn is_some(&self) -> bool {
        !self.is_none()
    }
}

impl Autoload {
    /// Construct an autoloader that loads from the paths given by `env_var_name`.
    pub fn new(env_var_name: &'static wstr) -> Self {
        Self {
            env_var_name,
            ..Default::default()
        }
    }

    /// Capture evaluated file identities and recursion guards for a native worker.
    #[cfg(windows)]
    pub fn snapshot(&self) -> AutoloadState {
        AutoloadState {
            loaded_files: self
                .autoloaded_files
                .iter()
                .map(|(name, identity)| (name.to_string(), identity.clone()))
                .collect(),
            in_progress: self
                .current_autoloading
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }

    /// Restore bookkeeping without probing files or evaluating any source.
    /// The next lookup compares its freshly discovered file to the identity
    /// that was actually evaluated before the snapshot was taken.
    #[cfg(windows)]
    pub fn restore(&mut self, state: AutoloadState) {
        self.autoloaded_files = state
            .loaded_files
            .into_iter()
            .map(|(name, identity)| (WString::from(name), identity))
            .collect();
        self.current_autoloading = state.in_progress.into_iter().map(WString::from).collect();
        self.cache = AutoloadFileCache::default();
    }

    /// Given a command, get a path to autoload.
    /// For example, if the environment variable is 'fish_function_path' and the command is 'foo',
    /// this will look for a file 'foo.fish' in one of the directories given by fish_function_path.
    /// If there is no such file, OR if the file has been previously resolved and is now unchanged,
    /// this will return none. But if the file is either new or changed, this will return the path.
    /// After returning a path, the command is marked in-progress until the caller calls
    /// mark_autoload_finished() with the same command. Note this does not actually execute any
    /// code; it is the caller's responsibility to load the file.
    #[cfg(not(windows))]
    pub fn resolve_command(&mut self, cmd: &wstr, env: &dyn Environment) -> AutoloadResult {
        let result = self.resolve_command_from_paths(
            cmd,
            env.get(self.env_var_name)
                .as_ref()
                .map(|var| var.as_list())
                .unwrap_or_default(),
        );
        match result {
            AutoloadResult::Path(AutoloadPath::OnDisk(ref path)) => {
                flogf!(
                    autoload,
                    "Loading %s from var %s from path %s",
                    cmd,
                    self.env_var_name,
                    path
                );
            }
            AutoloadResult::Path(AutoloadPath::Embedded(_)) => {
                flogf!(autoload, "Embedded: %s", cmd);
            }
            AutoloadResult::Loaded | AutoloadResult::Pending | AutoloadResult::None => {}
        }
        result
    }

    /// Helper to actually perform an autoload.
    /// This is a static function because it executes fish script, and so must be called without
    /// holding any particular locks.
    #[cfg(not(windows))]
    pub fn perform_autoload(path: &AutoloadPath, parser: &mut Parser) {
        // We do the useful part of what exec_subshell does ourselves
        // - we source the file.
        // We don't create a buffer or check ifs or create a read_limit
        let prev_statuses = parser.last_statuses();
        let mut parser = ScopeGuard::new(parser, |parser| parser.set_last_statuses(prev_statuses));
        match path {
            AutoloadPath::OnDisk(p) => {
                let script_source = L!("source ").to_owned() + &escape(p)[..];
                parser.eval(&script_source, &IoChain::new());
            }
            AutoloadPath::Embedded(name) => {
                use fish_widestring::bytes2wcstring;
                use std::sync::Arc;
                flogf!(autoload, "Loading embedded: %s", name);
                let emfile = Asset::get(name).expect("Embedded file not found");
                let src = bytes2wcstring(&emfile.data);
                let mut widename = L!("embedded:").to_owned();
                widename.push_str(name);
                let ret = parser.eval_file_wstr(src, Arc::new(widename), &IoChain::new(), None);
                if let Err(msg) = ret {
                    eprintf!("%s", msg);
                }
            }
        }
    }

    /// Mark that a command previously returned from path_to_autoload is finished autoloading.
    pub fn mark_autoload_finished(&mut self, cmd: &wstr) {
        let removed = self.current_autoloading.remove(cmd);
        assert!(removed, "cmd was not being autoloaded");
    }

    /// Return whether a command is currently being autoloaded.
    pub fn autoload_in_progress(&self, cmd: &wstr) -> bool {
        self.current_autoloading.contains(cmd)
    }

    /// Return whether a command could potentially be autoloaded.
    /// This does not actually mark the command as being autoloaded.
    pub fn can_autoload(&mut self, cmd: &wstr) -> bool {
        self.cache
            .check(self.env_var_name, cmd, true /* allow stale */)
            .is_some()
    }

    /// Check the current search paths without executing or marking a file loaded.
    /// Callers whose environment is not the POSIX EnvStack can supply its snapshot.
    pub fn can_autoload_from_paths(&mut self, cmd: &wstr, paths: &[WString]) -> bool {
        self.can_autoload_from_paths_with_embedded(cmd, paths, true)
    }

    /// Like `can_autoload_from_paths`, optionally excluding bundled script shims.
    pub fn can_autoload_from_paths_with_embedded(
        &mut self,
        cmd: &wstr,
        paths: &[WString],
        include_embedded: bool,
    ) -> bool {
        self.set_paths(paths, include_embedded);
        self.can_autoload(cmd)
    }

    /// Return whether autoloading has been attempted for a command.
    pub fn has_attempted_autoload(&self, cmd: &wstr) -> bool {
        self.cache.is_cached(cmd)
    }

    /// Return the names of all commands that have been autoloaded. Note this includes "in-flight"
    /// commands.
    pub fn get_autoloaded_commands(&self) -> Vec<WString> {
        let mut result = Vec::with_capacity(self.autoloaded_files.len());
        for k in self.autoloaded_files.keys() {
            result.push(k.to_owned());
        }
        // Sort the output to make it easier to test.
        result.sort();
        result
    }

    /// Mark that all autoloaded files have been forgotten.
    /// Future calls to path_to_autoload() will return previously-returned paths.
    pub fn clear(&mut self) {
        // Note there is no reason to invalidate the cache here.
        self.autoloaded_files.clear();
    }

    /// Invalidate any underlying cache.
    #[cfg(test)]
    fn invalidate_cache(&mut self) {
        self.cache =
            AutoloadFileCache::with_dirs(self.cache.dirs().to_owned(), self.cache.include_embedded);
    }

    fn set_paths(&mut self, paths: &[WString], include_embedded: bool) {
        // A path change invalidates lookups, but loaded file identities still let
        // us recognize an unchanged source reached through the new path list.
        if paths != self.cache.dirs() || include_embedded != self.cache.include_embedded {
            self.cache = AutoloadFileCache::with_dirs(paths.to_owned(), include_embedded);
        }
    }

    /// Resolve a function or completion using an ordered environment snapshot.
    /// A returned path is marked in progress until `mark_autoload_finished()`;
    /// the caller owns source evaluation and preservation of shell status.
    pub fn resolve_command_from_paths(&mut self, cmd: &wstr, paths: &[WString]) -> AutoloadResult {
        self.resolve_command_from_paths_with_embedded(cmd, paths, true)
    }

    /// Resolve with an explicit policy for bundled scripts. Native builtins can
    /// permit user files to override them without loading a POSIX script shim.
    pub fn resolve_command_from_paths_with_embedded(
        &mut self,
        cmd: &wstr,
        paths: &[WString],
        include_embedded: bool,
    ) -> AutoloadResult {
        // Are we currently in the process of autoloading this?
        if self.current_autoloading.contains(cmd) {
            return AutoloadResult::Pending;
        }

        self.set_paths(paths, include_embedded);

        // Do we have an entry to load?
        let Some(file) = self.cache.check(self.env_var_name, cmd, false) else {
            return AutoloadResult::None;
        };

        let file_id = match &file {
            AutoloadableFileInfo::OnDisk { file_id, .. } => Some(file_id),
            AutoloadableFileInfo::Embedded { .. } => None,
        };

        // Is this file the same as what we previously autoloaded?
        if let Some(loaded_file) = self.autoloaded_files.get(cmd) {
            if loaded_file.as_ref() == file_id {
                // The file has been autoloaded and is unchanged.
                return AutoloadResult::Loaded;
            }
        }

        // We're going to (tell our caller to) autoload this command.
        self.current_autoloading.insert(cmd.to_owned());
        self.autoloaded_files
            .insert(cmd.to_owned(), file_id.cloned());
        AutoloadResult::Path(match file {
            AutoloadableFileInfo::OnDisk { path, .. } => AutoloadPath::OnDisk(path),
            AutoloadableFileInfo::Embedded { path } => AutoloadPath::Embedded(path),
        })
    }
}

/// The time before we'll recheck an autoloaded file.
const AUTOLOAD_STALENESS_INTERVALL: u64 = 15;

/// Represents a file that we might want to autoload.
#[derive(Clone)]
enum AutoloadableFileInfo {
    /// An on-disk file.
    OnDisk { path: WString, file_id: FileId },
    /// An embedded file.
    Embedded { path: String },
}

// A timestamp is a monotonic point in time.
type Timestamp = time::Instant;
type MissesLruCache = LruCache<WString, Timestamp>;

#[derive(Clone)]
struct KnownFile {
    file: AutoloadableFileInfo,
    last_checked: Timestamp,
}

/// Class representing a cache of files that may be autoloaded.
/// This is responsible for performing cached accesses to a set of paths.
#[derive(Clone)]
struct AutoloadFileCache {
    /// The directories from which to load.
    dirs: Vec<WString>,

    /// Whether embedded scripts may be selected after user files.
    include_embedded: bool,

    /// Our LRU cache of checks that were misses.
    /// The key is the command, the  value is the time of the check.
    misses_cache: MissesLruCache,

    /// The set of files that we have returned to the caller, along with the time of the check.
    /// The key is the command (not the path).
    known_files: HashMap<WString, KnownFile>,
}

impl Default for AutoloadFileCache {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoloadFileCache {
    /// Initialize with a set of directories.
    fn with_dirs(dirs: Vec<WString>, include_embedded: bool) -> Self {
        Self {
            dirs,
            include_embedded,
            misses_cache: MissesLruCache::new(NonZeroUsize::new(1024).unwrap()),
            known_files: HashMap::new(),
        }
    }

    /// Initialize with empty directories.
    fn new() -> Self {
        Self::with_dirs(vec![], true)
    }

    /// Return the directories.
    fn dirs(&self) -> &[WString] {
        &self.dirs
    }

    /// Check if a command `cmd` can be loaded.
    /// If `allow_stale` is true, allow stale entries; otherwise discard them.
    /// This returns an autoloadable file, or none() if there is no such file.
    fn check(
        &mut self,
        env_var_name: &wstr,
        cmd: &wstr,
        allow_stale: bool,
    ) -> Option<AutoloadableFileInfo> {
        let asset_dir = match env_var_name {
            s if s == "fish_function_path" => Some(AssetDir::Functions),
            s if s == "fish_complete_path" => Some(AssetDir::Completions),
            _ => None,
        };

        // Check hits.
        if let Some(value) = self.known_files.get(cmd) {
            let embedded = matches!(value.file, AutoloadableFileInfo::Embedded { .. });
            if allow_stale
                || embedded
                || Self::is_fresh(value.last_checked, Self::current_timestamp())
            {
                // Re-use this cached hit.
                return Some(value.file.clone());
            }
            // The file is stale, remove it.
            self.known_files.remove(cmd);
        }

        // Check misses.
        if let Some(miss) = self.misses_cache.get(cmd) {
            if allow_stale || Self::is_fresh(*miss, Self::current_timestamp()) {
                // Re-use this cached miss.
                return None;
            }
            // The miss is stale, remove it.
            self.misses_cache.pop(cmd);
        }

        // We couldn't satisfy this request from the cache. Hit the disk.
        let file = self
            .locate_file(cmd, asset_dir, false)
            .or_else(|| {
                self.include_embedded
                    .then(|| self.locate_asset(cmd, asset_dir?))
                    .flatten()
            })
            .or_else(|| self.locate_file(cmd, asset_dir, true));
        if let Some(file) = file.as_ref() {
            let old_value = self.known_files.insert(
                cmd.to_owned(),
                KnownFile {
                    file: file.clone(),
                    last_checked: Self::current_timestamp(),
                },
            );
            assert!(
                old_value.is_none(),
                "Known files cache should not have contained this cmd"
            );
        } else {
            let old_value = self
                .misses_cache
                .put(cmd.to_owned(), Self::current_timestamp());
            assert!(
                old_value.is_none(),
                "Misses cache should not have contained this cmd",
            );
        }
        file
    }

    /// Return true if a command is cached (either as a hit or miss).
    fn is_cached(&self, cmd: &wstr) -> bool {
        self.known_files.contains_key(cmd) || self.misses_cache.contains(cmd)
    }

    /// Return the current timestamp.
    fn current_timestamp() -> Timestamp {
        Timestamp::now()
    }

    /// Return whether a timestamp is fresh enough to use.
    fn is_fresh(then: Timestamp, now: Timestamp) -> bool {
        let seconds = now.duration_since(then).as_secs();
        seconds < AUTOLOAD_STALENESS_INTERVALL
    }

    /// Attempt to find an autoloadable file by searching our path list for a given command.
    /// Return the file, or none() if none.
    fn locate_file(
        &self,
        cmd: &wstr,
        asset_dir: Option<AssetDir>,
        want_generated_completions: bool,
    ) -> Option<AutoloadableFileInfo> {
        // If the command is empty or starts with NULL (i.e. is empty as a path)
        // we'd try to source the *directory*, which exists.
        // So instead ignore these here.
        if cmd.is_empty() {
            return None;
        }
        if cmd.as_char_slice()[0] == '\0' {
            return None;
        }
        // Re-use the storage for path.
        let mut path;
        for dir in self.dirs() {
            if asset_dir == Some(AssetDir::Completions) {
                // HACK: Ignore generated_completions until we tried the embedded assets
                if dir.ends_with(L!("/generated_completions")) != want_generated_completions {
                    continue;
                }
            }
            // Construct the path as dir/cmd.fish
            path = dir.to_owned();
            path.push('/');
            path.push_utfstr(cmd);
            path.push_str(".fish");

            if let Ok(file_id) = file_id_for_path(&path) {
                // Found it.
                return Some(AutoloadableFileInfo::OnDisk { path, file_id });
            }
        }
        None
    }

    fn locate_asset(&self, cmd: &wstr, asset_dir: AssetDir) -> Option<AutoloadableFileInfo> {
        // POSIX cargo tests historically skip embedded functions because loading
        // them there can hang for reasons unrelated to the resolver. Native tests
        // need the same embedded definitions used by their evaluator.
        if cfg!(all(test, not(windows))) {
            return None;
        }
        let narrow = wcs2bytes(cmd);
        let cmdstr = std::str::from_utf8(&narrow).ok()?;
        let path = match asset_dir {
            AssetDir::Functions => "functions/".to_owned() + cmdstr + ".fish",
            AssetDir::Completions => "completions/".to_owned() + cmdstr + ".fish",
        };
        has_asset(&path).then_some(AutoloadableFileInfo::Embedded { path })
    }
}

#[cfg(test)]
mod portable_tests {
    use super::{Asset, Autoload, AutoloadPath, AutoloadResult, function_names};
    use fish_widestring::{L, WString, osstr2wcstring, wcs2osstring};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "fish-shared-autoload-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(error) => panic!("cannot create autoload fixture: {error}"),
                }
            }
        }

        fn paths(&self) -> Vec<WString> {
            vec![osstr2wcstring(self.0.as_os_str())]
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            // Each fixture only creates files directly in its own unique directory.
            if let Ok(entries) = std::fs::read_dir(&self.0) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        let _ = std::fs::remove_dir(entry.path());
                    } else {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    fn assert_disk(result: AutoloadResult, expected: &Path) {
        match result {
            AutoloadResult::Path(AutoloadPath::OnDisk(path)) => {
                assert_eq!(PathBuf::from(wcs2osstring(&path)), expected);
            }
            other => panic!("expected source {}, got {other:?}", expected.display()),
        }
    }

    #[test]
    fn paths_preserve_precedence_recursion_and_file_reload() {
        let first = Directory::new();
        let second = Directory::new();
        let first_source = first.0.join("greet.fish");
        let second_source = second.0.join("greet.fish");
        std::fs::write(&first_source, "first").unwrap();
        std::fs::write(&second_source, "other").unwrap();
        let mut paths = first.paths();
        paths.extend(second.paths());
        let mut autoload = Autoload::new(L!("test_paths"));

        // Introspection uses the same path cache without initiating evaluation.
        assert!(autoload.can_autoload_from_paths(L!("greet"), &paths));
        assert!(!autoload.autoload_in_progress(L!("greet")));
        assert_disk(
            autoload.resolve_command_from_paths(L!("greet"), &paths),
            &first_source,
        );
        assert!(matches!(
            autoload.resolve_command_from_paths(L!("greet"), &paths),
            AutoloadResult::Pending
        ));
        autoload.mark_autoload_finished(L!("greet"));
        assert!(matches!(
            autoload.resolve_command_from_paths(L!("greet"), &paths),
            AutoloadResult::Loaded
        ));

        // A different path must reload even with identically sized files.
        paths.reverse();
        assert_disk(
            autoload.resolve_command_from_paths(L!("greet"), &paths),
            &second_source,
        );
        autoload.mark_autoload_finished(L!("greet"));
        std::fs::write(&second_source, "changed definition").unwrap();
        autoload.invalidate_cache();
        assert_disk(
            autoload.resolve_command_from_paths(L!("greet"), &paths),
            &second_source,
        );
        autoload.mark_autoload_finished(L!("greet"));

        let mut cloned = autoload.clone();
        assert!(matches!(
            cloned.resolve_command_from_paths(L!("greet"), &paths),
            AutoloadResult::Loaded
        ));
        cloned.clear();
        assert_disk(
            cloned.resolve_command_from_paths(L!("greet"), &paths),
            &second_source,
        );
        assert!(matches!(
            autoload.resolve_command_from_paths(L!("greet"), &paths),
            AutoloadResult::Loaded
        ));
    }

    #[test]
    fn path_changes_invalidate_misses_and_empty_names_are_ignored() {
        let first = Directory::new();
        let second = Directory::new();
        let source = second.0.join("later.fish");
        std::fs::write(&source, "function later; end").unwrap();
        let mut autoload = Autoload::new(L!("test_paths"));
        assert!(!autoload.can_autoload_from_paths(L!("later"), &first.paths()));
        assert!(autoload.has_attempted_autoload(L!("later")));
        assert!(autoload.can_autoload_from_paths(L!("later"), &second.paths()));
        assert_disk(
            autoload.resolve_command_from_paths(L!("later"), &second.paths()),
            &source,
        );
        assert!(matches!(
            autoload.resolve_command_from_paths(L!(""), &second.paths()),
            AutoloadResult::None
        ));
    }

    #[test]
    fn embedded_assets_are_shared_with_both_evaluators() {
        let alias = Asset::get("functions/alias.fish").unwrap();
        assert!(String::from_utf8_lossy(&alias.data).contains("function alias"));
    }

    #[test]
    fn enumeration_filters_names_without_evaluating_sources() {
        let directory = Directory::new();
        for name in [
            "shared_test_東京.fish",
            "_shared_test_hidden.fish",
            ".fish",
            "wrong.txt",
        ] {
            std::fs::write(directory.0.join(name), "this is deliberately invalid Fish").unwrap();
        }
        std::fs::create_dir(directory.0.join("shared_test_directory.fish")).unwrap();
        let visible = function_names(&directory.paths(), false);
        assert!(visible.contains(L!("shared_test_東京")));
        assert!(visible.contains(L!("alias")));
        assert!(!visible.contains(L!("_shared_test_hidden")));
        assert!(!visible.contains(L!("shared_test_directory")));
        assert!(!visible.contains(L!("")));
        assert!(!visible.contains(L!("wrong")));
        assert!(function_names(&directory.paths(), true).contains(L!("_shared_test_hidden")));
    }

    #[cfg(not(windows))]
    #[test]
    fn enumeration_preserves_non_unicode_posix_filenames() {
        use std::os::unix::ffi::OsStringExt as _;
        let directory = Directory::new();
        let filename = std::ffi::OsString::from_vec(b"shared_test_\xff.fish".to_vec());
        std::fs::write(directory.0.join(filename), "function name; end").unwrap();
        let expected = fish_widestring::bytes2wcstring(b"shared_test_\xff");
        assert!(function_names(&directory.paths(), false).contains(&expected));
    }

    #[cfg(windows)]
    #[test]
    fn disk_functions_override_embedded_assets() {
        let directory = Directory::new();
        let source = directory.0.join("alias.fish");
        std::fs::write(&source, "function alias; echo override; end").unwrap();
        let mut autoload = Autoload::new(L!("fish_function_path"));
        assert_disk(
            autoload.resolve_command_from_paths(L!("alias"), &directory.paths()),
            &source,
        );
        autoload.mark_autoload_finished(L!("alias"));
        assert!(matches!(
            autoload.resolve_command_from_paths(L!("alias"), &[]),
            AutoloadResult::Path(AutoloadPath::Embedded(path))
                if path == "functions/alias.fish"
        ));
        autoload.mark_autoload_finished(L!("alias"));
        // A disk-only request must not reuse an embedded cache entry.
        assert!(matches!(
            autoload.resolve_command_from_paths_with_embedded(L!("alias"), &[], false),
            AutoloadResult::None
        ));
        assert_disk(
            autoload.resolve_command_from_paths_with_embedded(
                L!("alias"),
                &directory.paths(),
                false,
            ),
            &source,
        );
    }

    #[cfg(windows)]
    #[test]
    fn worker_snapshots_keep_original_file_identity_and_recursion_state() {
        let directory = Directory::new();
        let source = directory.0.join("worker_function.fish");
        let paths = directory.paths();
        std::fs::write(&source, "first definition").unwrap();
        let mut parent = Autoload::new(L!("fish_function_path"));
        assert_disk(
            parent.resolve_command_from_paths(L!("worker_function"), &paths),
            &source,
        );

        // A pipeline created while sourcing a file must retain its recursion guard.
        let encoded = bincode::serialize(&parent.snapshot()).unwrap();
        let mut pending = Autoload::new(L!("fish_function_path"));
        pending.restore(bincode::deserialize(&encoded).unwrap());
        assert!(matches!(
            pending.resolve_command_from_paths(L!("worker_function"), &paths),
            AutoloadResult::Pending
        ));
        pending.mark_autoload_finished(L!("worker_function"));
        assert!(matches!(
            pending.resolve_command_from_paths(L!("worker_function"), &paths),
            AutoloadResult::Loaded
        ));

        parent.mark_autoload_finished(L!("worker_function"));
        let encoded = bincode::serialize(&parent.snapshot()).unwrap();
        let mut unchanged = Autoload::new(L!("fish_function_path"));
        unchanged.restore(bincode::deserialize(&encoded).unwrap());
        assert!(matches!(
            unchanged.resolve_command_from_paths(L!("worker_function"), &paths),
            AutoloadResult::Loaded
        ));

        // A file changed after the parent loaded it must still be evaluated by
        // the worker; restore must not associate the old body with this identity.
        std::fs::write(&source, "a changed definition with a different length").unwrap();
        let mut changed = Autoload::new(L!("fish_function_path"));
        changed.restore(bincode::deserialize(&encoded).unwrap());
        assert_disk(
            changed.resolve_command_from_paths(L!("worker_function"), &paths),
            &source,
        );
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::{Autoload, AutoloadResult};
    use crate::prelude::*;
    use crate::tests::prelude::*;
    use assert_matches::assert_matches;

    #[test]
    #[serial]
    fn test_autoload() {
        test_init();
        use crate::fds::wopen_cloexec;
        use fish_widestring::wcs2zstring;
        use nix::fcntl::OFlag;

        macro_rules! run {
            ( $fmt:expr $(, $arg:expr )* $(,)? ) => {
                let cmd = wcs2zstring(&sprintf!($fmt $(, $arg)*));
                let status = unsafe { libc::system(cmd.as_ptr()) };
                assert_eq!(status, 0);
            };
        }

        fn touch_file(path: &wstr) {
            use nix::sys::stat::Mode;
            use std::io::Write as _;

            let mut file = wopen_cloexec(
                path,
                OFlag::O_RDWR | OFlag::O_CREAT,
                Mode::from_bits_truncate(0o666),
            )
            .unwrap();
            file.write_all(b"Hello").unwrap();
        }

        let p1 = fish_tempfile::new_dir().unwrap();
        let p1 = WString::from(p1.path().to_str().unwrap());
        let p2 = fish_tempfile::new_dir().unwrap();
        let p2 = WString::from(p2.path().to_str().unwrap());

        let paths = &[p1.clone(), p2.clone()];
        let mut autoload = Autoload::new(L!("test_var"));
        assert!(
            autoload
                .resolve_command_from_paths(L!("file1"), paths)
                .is_none()
        );
        assert!(
            autoload
                .resolve_command_from_paths(L!("nothing"), paths)
                .is_none()
        );
        assert!(autoload.get_autoloaded_commands().is_empty());

        run!("touch %s/file1.fish", p1);
        run!("touch %s/file2.fish", p2);
        autoload.invalidate_cache();

        assert!(!autoload.autoload_in_progress(L!("file1")));
        assert_matches!(
            autoload.resolve_command_from_paths(L!("file1"), paths),
            AutoloadResult::Path(_)
        );
        assert_matches!(
            autoload.resolve_command_from_paths(L!("file1"), paths),
            AutoloadResult::Pending
        );
        assert!(autoload.autoload_in_progress(L!("file1")));
        assert_eq!(autoload.get_autoloaded_commands(), vec![L!("file1")]);
        autoload.mark_autoload_finished(L!("file1"));
        assert!(!autoload.autoload_in_progress(L!("file1")));
        assert_eq!(autoload.get_autoloaded_commands(), vec![L!("file1")]);

        assert_matches!(
            autoload.resolve_command_from_paths(L!("file1"), paths),
            AutoloadResult::Loaded
        );
        assert!(
            autoload
                .resolve_command_from_paths(L!("nothing"), paths)
                .is_none()
        );
        assert!(
            autoload
                .resolve_command_from_paths(L!("file2"), paths)
                .is_some()
        );
        assert_matches!(
            autoload.resolve_command_from_paths(L!("file2"), paths),
            AutoloadResult::Pending
        );
        autoload.mark_autoload_finished(L!("file2"));
        assert_matches!(
            autoload.resolve_command_from_paths(L!("file2"), paths),
            AutoloadResult::Loaded
        );
        assert_eq!(
            autoload.get_autoloaded_commands(),
            vec![L!("file1"), L!("file2")]
        );

        autoload.clear();
        assert!(
            autoload
                .resolve_command_from_paths(L!("file1"), paths)
                .is_some()
        );
        autoload.mark_autoload_finished(L!("file1"));
        assert_matches!(
            autoload.resolve_command_from_paths(L!("file1"), paths),
            AutoloadResult::Loaded
        );
        assert!(
            autoload
                .resolve_command_from_paths(L!("nothing"), paths)
                .is_none()
        );
        assert!(
            autoload
                .resolve_command_from_paths(L!("file2"), paths)
                .is_some()
        );
        assert_matches!(
            autoload.resolve_command_from_paths(L!("file2"), paths),
            AutoloadResult::Pending
        );
        autoload.mark_autoload_finished(L!("file2"));

        assert_matches!(
            autoload.resolve_command_from_paths(L!("file1"), paths),
            AutoloadResult::Loaded
        );
        touch_file(&sprintf!("%s/file1.fish", p1));
        autoload.invalidate_cache();
        assert!(
            autoload
                .resolve_command_from_paths(L!("file1"), paths)
                .is_some()
        );
        autoload.mark_autoload_finished(L!("file1"));
    }
}
