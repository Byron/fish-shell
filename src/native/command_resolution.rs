//! Windows executable lookup shared by execution and command queries.
//!
//! Resolve before spawning: a selected executable which cannot be started must
//! keep its error instead of silently becoming a bundled utility invocation.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;

pub(super) fn is_explicit(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.contains(['/', '\\', ':'])
}

/// Return real filesystem candidates in PATH order. An empty PATH component
/// denotes the current directory; Windows' implicit executable-directory and
/// system-directory search never runs ahead of PATH.
pub(super) fn paths(
    name: &OsStr,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> io::Result<Vec<PathBuf>> {
    search(name, path, pathext, false)
}

pub(super) fn first(
    name: &OsStr,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> io::Result<Option<PathBuf>> {
    Ok(search(name, path, pathext, true)?.into_iter().next())
}

fn search(
    name: &OsStr,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
    first_only: bool,
) -> io::Result<Vec<PathBuf>> {
    let explicit = is_explicit(name);
    let directories = if explicit {
        vec![PathBuf::new()]
    } else {
        path.map(std::env::split_paths)
            .into_iter()
            .flatten()
            .collect()
    };
    let extensions = pathext
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsStr::new(".COM;.EXE;.BAT;.CMD"))
        .to_string_lossy();
    let mut found = Vec::new();
    for directory in directories {
        let base = directory.join(name);
        let suffixes = std::iter::once("").chain(extensions.split(';').filter(|s| !s.is_empty()));
        for suffix in suffixes {
            let mut candidate: OsString = base.as_os_str().to_owned();
            candidate.push(suffix);
            let candidate = PathBuf::from(candidate);
            match std::fs::symlink_metadata(&candidate) {
                // Preserve dangling symlinks too: invoking a file that is
                // present but broken must not activate an unrelated fallback.
                Ok(metadata) if !metadata.is_dir() => {
                    found.push(absolute(candidate)?);
                    if first_only {
                        return Ok(found);
                    }
                    break;
                }
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
        }
    }
    Ok(found)
}

fn absolute(path: PathBuf) -> io::Result<PathBuf> {
    std::path::absolute(path)
}

/// Exact PATH spelling wins when a Windows environment contains both PATH and
/// Path. Fish exports PATH in that situation as well.
pub(super) fn environment<'a>(
    values: &'a std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
    name: &[u8],
) -> Option<&'a [u8]> {
    values.get(name).map(Vec::as_slice).or_else(|| {
        values
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_slice())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_names_include_drive_relative_paths() {
        assert!(!is_explicit(OsStr::new("cat")));
        assert!(!is_explicit(OsStr::new("cat.exe")));
        for name in [
            "./cat",
            ".\\cat",
            "C:cat",
            "C:\\bin\\cat",
            "\\\\server\\cat",
        ] {
            assert!(is_explicit(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn exact_environment_spelling_wins() {
        let values = std::collections::BTreeMap::from([
            (b"Path".to_vec(), b"other".to_vec()),
            (b"PATH".to_vec(), b"selected".to_vec()),
        ]);
        assert_eq!(environment(&values, b"PATH"), Some(b"selected".as_slice()));
    }
}
