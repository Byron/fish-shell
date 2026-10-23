//! Private directories whose owners can clean up terminated interpreter workers.
use rash::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) struct Directory(PathBuf);

impl Directory {
    pub(super) fn create(parent: &Path) -> Result<Self> {
        let parent = if parent.is_absolute() {
            parent.to_owned()
        } else {
            std::env::current_dir()?.join(parent)
        };
        for _ in 0..64 {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = parent.join(format!(
                "fish-native-psub-{}-{stamp}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            #[allow(unused_mut)]
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::new("unable to reserve a temporary directory"))
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }

    #[cfg(windows)]
    pub(super) fn persist(mut self) -> PathBuf {
        std::mem::take(&mut self.0)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // The path is the exact directory reserved above. remove_dir_all does
        // not follow symlinks that a consumer might have placed inside it.
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// A worker removes its own scratch root after ordinary completion, even if
/// its parent disowned it and exited. Never remove files still held by a
/// detached inner job; the parent's recursive guard handles forced termination.
pub(super) struct EmptyDirectory(pub(super) PathBuf);

impl Drop for EmptyDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_cleanup_removes_only_empty_scratch_directories() {
        let directory = Directory::create(&std::env::temp_dir()).unwrap();
        let file = directory.path().join("still-in-use");
        std::fs::write(&file, b"detached consumer").unwrap();
        drop(EmptyDirectory(directory.path().to_owned()));
        assert!(file.exists());
        std::fs::remove_file(file).unwrap();
        drop(EmptyDirectory(directory.path().to_owned()));
        assert!(!directory.path().exists());
    }
}
