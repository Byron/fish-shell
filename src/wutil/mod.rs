pub mod dir_iter;
mod errors;
mod fileid;
mod hex_float;
#[macro_use]
pub mod printf;
pub mod wcstod;
pub mod wcstoi;

pub use crate::path_lexical::{normalize_path, wbasename, wdirname};
pub use errors::*;
pub use fileid::*;
pub use printf::{eprintf, fprintf, printf, sprintf};
pub use wcstoi::{
    fish_wcstoi, fish_wcstol, fish_wcstol_radix, fish_wcstoul, wcstoi, wcstoi_opts, wcstoi_partial,
};

use crate::{fds::BorrowedFdFile, flog, signal::SigChecker};
use fish_util::perror;
use fish_widestring::{
    IntoCharIter, WExt as _, WString, bytes2wcstring, fish_reserved_codepoint, osstr2wcstring,
    str2bytes_callback, wcs2osstring, wcs2zstring, wstr,
};
use nix::unistd::{self, AccessFlags};
use std::{
    ffi::OsStr,
    fs::{self, canonicalize},
    io,
    os::unix::prelude::*,
};

/// Wide character version of opendir(). Note that opendir() is guaranteed to set close-on-exec by
/// POSIX (hooray).
pub fn wopendir(name: &wstr) -> *mut libc::DIR {
    let tmp = wcs2zstring(name);
    unsafe { libc::opendir(tmp.as_ptr()) }
}

/// Wide character version of stat().
pub fn wstat(file_name: &wstr) -> io::Result<fs::Metadata> {
    let tmp = wcs2osstring(file_name);
    fs::metadata(tmp)
}

/// Wide character version of lstat().
pub fn lwstat(file_name: &wstr) -> io::Result<fs::Metadata> {
    let tmp = wcs2osstring(file_name);
    fs::symlink_metadata(tmp)
}

/// Cover over fstat().
pub fn fstat(fd: impl AsRawFd) -> io::Result<fs::Metadata> {
    let file = unsafe { BorrowedFdFile::from_raw_fd(fd.as_raw_fd()) };
    file.metadata()
}

/// Wide character version of access().
pub fn waccess(file_name: &wstr, amode: AccessFlags) -> nix::Result<()> {
    let tmp = wcs2osstring(file_name);
    nix::unistd::access(tmp.as_os_str(), amode)
}

/// Wide character version of unlink().
pub fn wunlink(file_name: &wstr) -> io::Result<()> {
    let tmp = wcs2osstring(file_name);
    fs::remove_file(tmp)
}

pub fn perror_nix(s: &str, e: nix::errno::Errno) {
    eprintf!("%s: %s\n", s, e.desc());
}

pub fn perror_io(s: &str, e: &io::Error) {
    eprintf!("%s: %s\n", s, e);
}

/// Wide character version of getcwd().
pub fn wgetcwd() -> WString {
    match std::env::current_dir() {
        Ok(cwd) => osstr2wcstring(cwd),
        Err(e) => {
            flog!(error, "std::env::current_dir() failed with error:", e);
            WString::new()
        }
    }
}

/// Wide character version of readlink().
pub fn wreadlink(file_name: &wstr) -> Option<WString> {
    _ = lwstat(file_name).ok()?;
    match fs::read_link(wcs2osstring(file_name)) {
        Ok(target) => Some(osstr2wcstring(target)),
        Err(e) => {
            perror_io("readlink", &e);
            None
        }
    }
}

/// Wide character realpath. The last path component does not need to be valid.
pub fn wrealpath(pathname: &wstr) -> io::Result<WString> {
    if pathname.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty path"));
    }

    let mut narrow_path: Vec<u8> = wcs2zstring(pathname).into();

    // Strip trailing slashes. This is treats "/a//" as equivalent to "/a" if /a is a non-directory.
    while narrow_path.len() > 1 && narrow_path[narrow_path.len() - 1] == b'/' {
        narrow_path.pop();
    }

    let narrow_res = canonicalize(OsStr::from_bytes(&narrow_path));

    let real_path = if let Ok(result) = narrow_res {
        result.into_os_string().into_vec()
    } else {
        // Check if everything up to the last path component is valid.
        let pathsep_idx = narrow_path.iter().rposition(|&c| c == b'/');

        if pathsep_idx == Some(0) {
            // If the only pathsep is the first character then it's an absolute path with a
            // single path component and thus doesn't need conversion.
            narrow_path
        } else {
            // Only call realpath() on the portion up to the last component.
            let narrow_result = if let Some(pathsep_idx) = pathsep_idx {
                // Only call realpath() on the portion up to the last component.
                canonicalize(OsStr::from_bytes(&narrow_path[0..pathsep_idx]))
            } else {
                // If there is no "/", this is a file in $PWD, so give the realpath to that.
                canonicalize(".")
            }?;

            let pathsep_idx = pathsep_idx.map_or(0, |idx| idx + 1);

            let mut real_path = narrow_result.into_os_string().into_vec();

            // This test is to deal with cases such as /../../x => //x.
            if real_path.len() > 1 {
                real_path.push(b'/');
            }

            real_path.extend_from_slice(&narrow_path[pathsep_idx..]);

            real_path
        }
    };

    Ok(bytes2wcstring(&real_path))
}

/// Given an input path `path` and a working directory `wd`, do a "normalizing join" in a way
/// appropriate for cd. That is, return effectively wd + path while resolving leading ../s from
/// path. The intent here is to allow 'cd' out of a directory which may no longer exist, without
/// allowing 'cd' into a directory that may not exist; see #5341.
pub fn path_normalize_for_cd(wd: &wstr, path: &wstr) -> WString {
    use std::collections::VecDeque;

    // Fast paths.
    const SEP: char = '/';
    assert!(
        wd.as_char_slice().first() == Some(&'/') && wd.as_char_slice().last() == Some(&'/'),
        "Invalid working directory, it must start and end with /"
    );
    if path.is_empty() {
        return wd.to_owned();
    } else if path.as_char_slice().first() == Some(&SEP) {
        return path.to_owned();
    } else if path.as_char_slice().first() != Some(&'.') {
        return wd.to_owned() + path;
    }

    // Split our strings by the sep.
    let mut wd_comps: VecDeque<_> = wd
        .split(SEP)
        // Remove empty segments from wd_comps, especially leading/trailing empties.
        .filter(|p| !p.is_empty())
        .collect();
    let mut path_comps = path.split(SEP).peekable();

    // Erase leading . and .. components from path_comps, popping from wd_comps as we go.
    while let Some(comp) = path_comps.peek() {
        #[allow(clippy::if_same_then_else)]
        if comp.is_empty() || comp == "." {
            path_comps.next();
        } else if comp == ".." && wd_comps.pop_back().is_some() {
            path_comps.next();
        } else {
            break;
        }
    }

    // Append un-erased elements to wd_comps and join them, prepending with a leading /
    let mut paths = wd_comps;
    paths.extend(path_comps);
    let mut result =
        WString::with_capacity(paths.iter().fold(paths.len() + 1, |sum, s| sum + s.len()));
    for p in paths.iter() {
        result.push(SEP);
        result.push_utfstr(*p);
    }
    if result.is_empty() {
        result.push(SEP);
    }
    result
}

/// Write a wide string to a file descriptor. This avoids doing any additional allocation.
/// Returns nothing when interrupted by ctrl-c or HUP.
pub fn unescape_bytes_and_write_to_fd(input: impl IntoCharIter, fd: RawFd) -> Option<usize> {
    // Accumulate data in a local buffer.
    let mut accum = [0u8; 512];
    let mut accumlen = 0;
    let accum_capacity = accum.len();

    // Helper to perform a write to 'fd', looping as necessary.
    // Return true on success, false on error.
    let mut total_written = 0;

    fn do_write(
        sigcheck: &mut SigChecker,
        fd: RawFd,
        total_written: &mut usize,
        mut buf: &[u8],
    ) -> bool {
        while !buf.is_empty() {
            let amt = match unistd::write(unsafe { BorrowedFd::borrow_raw(fd) }, buf) {
                Ok(amt) => amt,
                Err(err) => {
                    // Some of our builtins emit multiple screens worth of data sent to a pager (the primary
                    // example being the `history` builtin) and receiving SIGINT should be considered normal and
                    // non-exceptional (user request to abort via Ctrl-C), meaning we shouldn't print an error.
                    //
                    // We have two options here: we can either return false without setting errored_ to
                    // true (*this* write will be silently aborted but the onus is on the caller to check
                    // the return value and skip future calls to `append()`) or we can flag the entire
                    // output stream as errored, causing us to both return false and skip any future writes.
                    // We're currently going with the latter, especially seeing as no callers currently
                    // check the result of `append()` (since it was always a void function before).
                    match err {
                        nix::errno::Errno::EINTR => {
                            if !sigcheck.check() {
                                continue;
                            }
                        }
                        nix::errno::Errno::EPIPE => (),
                        _ => perror("write"),
                    }
                    return false;
                }
            };
            *total_written += amt;
            assert!(amt <= buf.len(), "Wrote more than requested");
            buf = &buf[amt..];
        }
        true
    }

    // Helper to flush the accumulation buffer.
    let flush_accum = |sigcheck: &mut SigChecker,
                       total_written: &mut usize,
                       accum: &[u8],
                       accumlen: &mut usize| {
        if !do_write(sigcheck, fd, total_written, &accum[..*accumlen]) {
            return false;
        }
        *accumlen = 0;
        true
    };

    let mut sigcheck = SigChecker::new_sighupintterm();
    let mut success = str2bytes_callback(input, |buff: &[u8]| {
        if buff.len() + accumlen > accum_capacity {
            // We have to flush.
            if !flush_accum(&mut sigcheck, &mut total_written, &accum, &mut accumlen) {
                return false;
            }
        }
        if buff.len() + accumlen <= accum_capacity {
            // Accumulate more.
            accum[accumlen..(accumlen + buff.len())].copy_from_slice(buff);
            accumlen += buff.len();
            true
        } else {
            // Too much data to even fit, just write it immediately.
            do_write(&mut sigcheck, fd, &mut total_written, buff)
        }
    });
    // Flush any remaining.
    if success {
        success = flush_accum(&mut sigcheck, &mut total_written, &accum, &mut accumlen);
    }
    if success { Some(total_written) } else { None }
}

const PUA1_START: char = '\u{E000}';
const PUA1_END: char = '\u{F900}';
// const PUA2_START: char = '\u{F0000}';
// const PUA2_END: char = '\u{FFFFE}';
// const PUA3_START: char = '\u{100000}';
// const PUA3_END: char = '\u{10FFFE}';

/// Return one if the code point is in a Unicode private use area.
pub(crate) fn fish_is_pua(c: char) -> bool {
    PUA1_START <= c && c < PUA1_END
}

/// We need this because there are too many implementations that don't return the proper answer for
/// some code points. See issue #3050.
pub fn fish_iswalnum(c: char) -> bool {
    !fish_reserved_codepoint(c) && !fish_is_pua(c) && c.is_alphanumeric()
}

/// Given that `cursor` is a pointer into `base`, return the offset in characters.
/// This emulates C pointer arithmetic:
///    `wstr_offset_in(cursor, base)` is equivalent to C++ `cursor - base`.
pub fn wstr_offset_in(cursor: &wstr, base: &wstr) -> usize {
    let cursor = cursor.as_slice();
    let base = base.as_slice();
    // cursor may be a zero-length slice at the end of base,
    // which base.as_ptr_range().contains(cursor.as_ptr()) will reject.
    let base_range = base.as_ptr_range();
    let curs_range = cursor.as_ptr_range();
    assert!(
        base_range.start <= curs_range.start && curs_range.end <= base_range.end,
        "cursor should be a subslice of base"
    );
    let offset = unsafe { cursor.as_ptr().offset_from(base.as_ptr()) };
    assert!(offset >= 0, "offset should be non-negative");
    offset as usize
}

#[cfg(test)]
mod tests {
    use super::{unescape_bytes_and_write_to_fd, wstr_offset_in};
    use crate::tests::prelude::*;
    use fish_widestring::bytes2wcstring;
    use rand::RngExt as _;
    use std::{
        fs::OpenOptions,
        io::{Read as _, Seek as _},
        os::{fd::AsRawFd as _, unix::fs::OpenOptionsExt as _},
    };

    mod test_path_normalize_for_cd {
        use super::super::path_normalize_for_cd;
        use fish_widestring::L;

        #[test]
        fn relative_path() {
            let wd = L!("/home/user/");
            let path = L!("projects");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/home/user/projects"),
                "Normalized path for ({wd}, {path})"
            );
        }

        #[test]
        fn absolute_path() {
            let wd = L!("/home/user/");
            let path = L!("/etc");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/etc"),
                "Normalized path for ({wd}, {path})"
            );
        }

        #[test]
        fn parent_directory() {
            let wd = L!("/home/user/projects/");
            let path = L!("../docs");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/home/user/docs"),
                "Normalized path for ({wd}, {path})"
            );
        }

        #[test]
        fn current_directory() {
            let wd = L!("/home/user/");
            let path = L!("./");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/home/user"),
                "Normalized path for ({wd}, {path})"
            );
        }

        #[test]
        fn nested_parent_directory() {
            let wd = L!("/home/user/projects/");
            let path = L!("../../");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/home"),
                "Normalized path for ({wd}, {path})"
            );
        }

        #[test]
        fn complex_path() {
            let wd = L!("/home/user/projects/");
            let path = L!("./../other/projects/./.././../docs");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/home/user/other/projects/./.././../docs"),
                "Normalized path for ({wd}, {path})"
            );
        }

        #[test]
        fn root_directory() {
            let wd = L!("/");
            let path = L!("..");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/.."),
                "Normalized path for ({wd}, {path})"
            );
        }

        #[test]
        fn up_to_root_directory() {
            let wd = L!("/foo/");
            let path = L!("..");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/"),
                "Normalized path for ({wd}, {path})"
            );
        }

        #[test]
        fn empty_path() {
            let wd = L!("/home/user/");
            let path = L!("");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/home/user/"),
                "Normalized path for ({wd}, {path})"
            );
        }

        #[test]
        fn trailing_slash() {
            let wd = L!("/home/user/projects/");
            let path = L!("docs/");
            assert_eq!(
                path_normalize_for_cd(wd, path),
                L!("/home/user/projects/docs/"),
                "Normalized path for ({wd}, {path})"
            );
        }
    }

    #[test]
    #[serial]
    fn test_wwrite_to_fd() {
        test_init();
        let temp_file = fish_tempfile::new_file().unwrap();
        let mut rng = rand::rng();
        let sizes = [1, 2, 3, 5, 13, 23, 64, 128, 255, 4096, 4096 * 2];
        for &size in &sizes {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o666)
                .open(temp_file.path())
                .unwrap();
            let mut input = Vec::new();
            for _i in 0..size {
                input.push(rng.random());
            }

            let amt =
                unescape_bytes_and_write_to_fd(&bytes2wcstring(&input), file.as_raw_fd()).unwrap();
            assert_eq!(amt, input.len());

            file.seek(std::io::SeekFrom::Start(0)).unwrap();

            let mut contents = vec![];
            file.read_to_end(&mut contents).unwrap();
            assert_eq!(&contents, &input);
        }
    }

    #[test]
    fn test_wstr_offset_in() {
        use fish_widestring::L;
        let base = L!("hello world");
        assert_eq!(wstr_offset_in(&base[6..], base), 6);
        assert_eq!(wstr_offset_in(&base[0..], base), 0);
        assert_eq!(wstr_offset_in(&base[6..], &base[6..]), 0);
        assert_eq!(wstr_offset_in(&base[base.len()..], base), base.len());
    }
}
