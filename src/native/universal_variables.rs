//! Native universal-variable transactions using Fish's shared on-disk codec.
use super::fish::Variable;
use super::storage;
use crate::universal_variables::{Codec, Entry, Flags, Format, MAX_READ_SIZE, Table};
use fish_widestring::WString;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Store {
    path: Option<PathBuf>,
    pub(super) vars: BTreeMap<String, Variable>,
    #[serde(default)]
    last_error: Option<String>,
}

pub(super) struct Change {
    pub name: String,
    pub erased: bool,
}

impl Store {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            ..Default::default()
        }
    }
    pub(super) fn enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Cache diagnostics until recovery so a bad startup file does not flood
    /// stderr before every subsequent command. The configured path is retained.
    pub(super) fn diagnostic(&mut self, error: &io::Error) -> Option<String> {
        let message = format!(
            "universal variables: {}: {error}",
            self.path.as_deref().unwrap_or(Path::new("")).display()
        );
        if self.last_error.as_ref() == Some(&message) {
            return None;
        }
        self.last_error = Some(message.clone());
        Some(message)
    }

    pub(super) fn refresh(&mut self) -> io::Result<Vec<Change>> {
        let Some(path) = &self.path else {
            return Ok(Vec::new());
        };
        // Every native write is an atomic replacement; readers see a complete
        // old or new file without creating a directory/lock just to read it.
        let (vars, _) = read(path)?;
        let changes = changes(&self.vars, &vars);
        self.vars = vars;
        self.last_error = None;
        Ok(changes)
    }

    /// Commit one explicit mutation, never a saved session snapshot. Workers
    /// carry no dirty queue, so stale worker state cannot replay earlier writes.
    #[cfg(test)]
    pub(super) fn update(
        &mut self,
        name: &str,
        operation: impl FnOnce(Option<&Variable>) -> Option<Variable>,
    ) -> io::Result<(bool, Vec<Change>)> {
        self.try_update(name, |old| Ok(operation(old)))
    }

    pub(super) fn try_update(
        &mut self,
        name: &str,
        operation: impl FnOnce(Option<&Variable>) -> io::Result<Option<Variable>>,
    ) -> io::Result<(bool, Vec<Change>)> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| io::Error::other("universal persistence is disabled"))?;
        let path = storage::resolved(path)?;
        let (existed, vars, remote_changes) = storage::locked(&path, || {
            let (mut vars, format) = read(&path)?;
            if format == Format::Future {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "file was written by a newer Fish; refusing to overwrite it",
                ));
            }
            let remote_changes = changes(&self.vars, &vars);
            let existed = vars.contains_key(name);
            let value = operation(vars.get(name))?;
            let changed = vars.get(name) != value.as_ref();
            match value {
                Some(value) => {
                    vars.insert(name.to_owned(), value);
                }
                None => {
                    vars.remove(name);
                }
            }
            if changed {
                let table: Table = vars
                    .iter()
                    .map(|(name, value)| {
                        (
                            WString::from(name.as_str()),
                            Entry {
                                values: value
                                    .values
                                    .iter()
                                    .map(|value| WString::from(value.as_str()))
                                    .collect(),
                                flags: Flags {
                                    exported: value.exported,
                                    pathvar: value.pathvar,
                                },
                            },
                        )
                    })
                    .collect();
                let mut invalid_name = false;
                let bytes = Codec::serialize_with_vars(&table, |_| invalid_name = true);
                if invalid_name {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "file contains an invalid variable name",
                    ));
                }
                if bytes.len() > MAX_READ_SIZE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "universal variables exceed 16 MiB",
                    ));
                }
                storage::rewrite(&path, |file| file.write_all(&bytes))?;
            }
            Ok((existed, vars, remote_changes))
        })?;
        // Update the cache before returning callbacks, after releasing the lock.
        self.vars = vars;
        self.last_error = None;
        Ok((existed, remote_changes))
    }
}

fn changes(old: &BTreeMap<String, Variable>, new: &BTreeMap<String, Variable>) -> Vec<Change> {
    old.keys()
        .chain(new.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|name| old.get(*name) != new.get(*name))
        .map(|name| Change {
            name: name.clone(),
            erased: !new.contains_key(name),
        })
        .collect()
}

fn read(path: &Path) -> io::Result<(BTreeMap<String, Variable>, Format)> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((BTreeMap::new(), Format::Fish_3_0));
        }
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_READ_SIZE as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_READ_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "universal variables exceed 16 MiB",
        ));
    }
    let mut vars = Table::new();
    let mut malformed = std::str::from_utf8(&bytes).is_err();
    let format = Codec::populate_variables(&bytes, &mut vars, |_| malformed = true);
    if malformed && format != Format::Future {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file contains malformed universal-variable records",
        ));
    }
    Ok((
        vars.into_iter()
            .map(|(name, entry)| {
                (
                    name.to_string(),
                    Variable {
                        values: entry
                            .values
                            .into_iter()
                            .map(|value| value.to_string())
                            .collect(),
                        exported: entry.flags.exported,
                        pathvar: entry.flags.pathvar,
                    },
                )
            })
            .collect(),
        format,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "fish-native-uvars-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed),
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("cannot create uvar fixture: {error}"),
                }
            }
        }
        fn path(&self) -> PathBuf {
            self.0.join("native_fish_variables")
        }
        fn store(&self) -> Store {
            Store::new(self.path())
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let root = self.0.canonicalize().unwrap();
            let temp = std::env::temp_dir().canonicalize().unwrap();
            assert_eq!(root.parent(), Some(temp.as_path()));
            let _ = fs::remove_dir_all(root);
        }
    }

    fn value(value: &str) -> Option<Variable> {
        Some(Variable {
            values: vec![value.into()],
            ..Default::default()
        })
    }

    #[test]
    fn absent_store_refresh_creates_nothing_and_is_retryable() {
        let directory = Directory::new();
        let path = directory.0.join("not-created/vars");
        let mut store = Store::new(path.clone());
        assert!(store.refresh().unwrap().is_empty());
        assert!(!path.parent().unwrap().exists());
        store.update("first", |_| value("saved")).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn stale_and_serialized_snapshots_never_replay_erased_values() {
        let directory = Directory::new();
        let mut first = directory.store();
        first.update("removed", |_| value("old")).unwrap();
        let mut stale = first.clone();
        let mut worker: Store = bincode::deserialize(&bincode::serialize(&first).unwrap()).unwrap();
        first.update("removed", |_| None).unwrap();
        stale.update("second", |_| value("two")).unwrap();
        worker.update("third", |_| value("three")).unwrap();
        first.refresh().unwrap();
        assert_eq!(
            first.vars.keys().map(String::as_str).collect::<Vec<_>>(),
            ["second", "third"]
        );
        assert_eq!(first.vars, worker.vars);
    }

    #[test]
    fn concurrent_key_updates_and_appends_use_fresh_locked_values() {
        let directory = Directory::new();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|index| {
                let barrier = barrier.clone();
                let mut store = directory.store();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .update(&format!("worker_{index}"), |_| value("present"))
                        .unwrap();
                    for item in 0..10 {
                        store
                            .update("shared", |old| {
                                let mut new = old.cloned().unwrap_or_default();
                                new.values.push(format!("{index}-{item}"));
                                Some(new)
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let mut store = directory.store();
        store.refresh().unwrap();
        assert_eq!(store.vars.len(), 5);
        let values = &store.vars["shared"].values;
        assert_eq!(values.len(), 40);
        assert_eq!(
            values
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            40
        );
    }

    #[test]
    fn unsupported_corrupt_and_oversized_files_are_preserved() {
        let directory = Directory::new();
        let mut store = directory.store();
        store.update("previous", |_| value("cached")).unwrap();
        let previous = store.vars.clone();
        for bytes in [
            b"# VERSION: 3.0\nSETUVAR valid:value\nSETUVAR malformed\n".to_vec(),
            b"# VERSION: 3.0\nSETUVAR bad:\xff\n".to_vec(),
            vec![b'x'; MAX_READ_SIZE + 1],
        ] {
            fs::write(directory.path(), &bytes).unwrap();
            assert!(store.refresh().is_err());
            assert!(store.update("new", |_| value("discarded")).is_err());
            assert_eq!(store.vars, previous);
            assert_eq!(fs::read(directory.path()).unwrap(), bytes);
        }
        let future = b"# VERSION: 99.0\nSETUVAR valid:value\nunknown future record\n";
        fs::write(directory.path(), future).unwrap();
        store.refresh().unwrap();
        assert_eq!(store.vars["valid"].values, ["value"]);
        assert!(store.update("new", |_| value("discarded")).is_err());
        assert_eq!(fs::read(directory.path()).unwrap(), future);
        fs::write(directory.path(), b"# VERSION: 3.0\n").unwrap();
        store.update("recovered", |_| value("saved")).unwrap();
        assert_eq!(store.vars.len(), 1);
    }

    #[test]
    fn read_only_file_loads_but_failed_replacement_retains_disk_and_cache() {
        let directory = Directory::new();
        let mut store = directory.store();
        store.update("previous", |_| value("cached")).unwrap();
        let before = fs::read(directory.path()).unwrap();
        let original_permissions = fs::metadata(directory.path()).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        fs::set_permissions(directory.path(), permissions).unwrap();
        let mut reader = directory.store();
        reader.refresh().unwrap();
        let failed = reader.update("previous", |_| value("new"));
        fs::set_permissions(directory.path(), original_permissions).unwrap();
        assert!(failed.is_err());
        assert_eq!(reader.vars, store.vars);
        assert_eq!(fs::read(directory.path()).unwrap(), before);
        assert_eq!(
            fs::read_dir(&directory.0).unwrap().count(),
            2,
            "failed temporary files must be cleaned up"
        );
        reader.update("previous", |_| value("recovered")).unwrap();
    }

    #[test]
    fn file_symlinks_keep_their_identity_including_dangling_targets() {
        let directory = Directory::new();
        let link = directory.0.join("link");
        if let Err(error) = std::os::windows::fs::symlink_file("native_fish_variables", &link) {
            if error.raw_os_error() == Some(1314) {
                return; // Creating symlinks requires Developer Mode or a privileged token.
            }
            panic!("cannot create test symlink: {error}");
        }
        let mut store = Store::new(link.clone());
        store.update("first", |_| value("one")).unwrap();
        store.update("second", |_| value("two")).unwrap();
        assert!(fs::symlink_metadata(link).unwrap().is_symlink());
        let mut direct = directory.store();
        direct.refresh().unwrap();
        assert_eq!(direct.vars, store.vars);
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 3);
    }
}
