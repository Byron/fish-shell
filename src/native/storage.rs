//! Stable locks and atomic replacement for native persistent state.
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn peer(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

pub(super) fn locked<T>(path: &Path, action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    // The data file is replaced atomically. Its lock would not protect the new
    // file after a rename, so every native writer locks this stable sidecar.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(peer(path, ".lock"))?;
    let mut lock = fd_lock::RwLock::new(file);
    let _guard = lock.write()?;
    action()
}

/// Resolve aliases before choosing lock and replacement paths. Follow dangling
/// file symlinks too: replacing the link itself would silently discard it.
pub(super) fn resolved(path: &Path) -> io::Result<PathBuf> {
    let mut path = path.to_owned();
    for _ in 0..40 {
        match fs::canonicalize(&path) {
            Ok(path) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("state file has no parent"))?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_symlink() => {
                let target = fs::read_link(&path)?;
                path = if target.is_absolute() {
                    target
                } else {
                    parent.join(target)
                };
            }
            Ok(_) => return Err(io::Error::other("unable to resolve state file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(parent)?;
                let parent = fs::canonicalize(parent)?;
                return Ok(parent.join(
                    path.file_name()
                        .ok_or_else(|| io::Error::other("state file has no name"))?,
                ));
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("too many state-file symlinks"))
}

pub(super) fn rewrite(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let (temporary, mut file) = loop {
        let temporary = peer(
            path,
            &format!(
                ".tmp.{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
        );
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = write(&mut file).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| replace(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both buffers are valid NUL-terminated UTF-16 for this call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
