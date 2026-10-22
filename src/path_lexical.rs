//! Lexical path operations shared by Fish's platform adapters.
//!
//! Keep paths as wide strings so POSIX byte escapes survive unchanged. Root and separator
//! policies differ by platform; component reduction does not consult the filesystem.
use fish_widestring::{L, WExt as _, WString, wstr};

/// Locate the final extension using a basename supplied by the platform adapter.
/// A leading dot and the special directory names do not introduce extensions.
pub(crate) fn extension_offset(path: &wstr, basename: &wstr) -> Option<usize> {
    if basename == "." || basename == ".." {
        return None;
    }
    basename
        .chars()
        .rposition(|c| c == '.')
        .filter(|&position| position != 0)
        .map(|position| position + path.len() - basename.len())
}

struct Root<'a> {
    prefix: &'a wstr,
    rooted: bool,
}

fn normalize_with_root(
    path: &wstr,
    root: Root<'_>,
    is_separator: fn(char) -> bool,
    separator: char,
) -> WString {
    let mut components: Vec<&wstr> = Vec::new();
    for chars in path.as_char_slice()[root.prefix.char_count()..].split(|c| is_separator(*c)) {
        let component = wstr::from_char_slice(chars);
        if component.is_empty() || component == "." {
            continue;
        } else if component != ".." {
            components.push(component);
        } else if components.last().is_some_and(|c| *c != "..") {
            components.pop();
        } else if !root.rooted {
            // Relative paths can walk above their starting directory; roots cannot.
            components.push(component);
        }
    }

    let mut result = WString::from_chars(
        root.prefix
            .chars()
            .map(|c| if is_separator(c) { separator } else { c })
            .collect::<Vec<_>>(),
    );
    for (index, component) in components.iter().enumerate() {
        if index > 0 || (root.rooted && result.as_char_slice().last() != Some(&separator)) {
            result.push(separator);
        }
        result.push_utfstr(component);
    }
    if components.is_empty() && !root.rooted {
        // This also preserves a drive-relative current directory as C:., never C:\.
        result.push('.');
    }
    result
}

/// Collapse redundant POSIX separators and dot components without following symlinks.
#[cfg_attr(windows, allow(dead_code))]
pub fn normalize_path(path: &wstr, allow_leading_double_slashes: bool) -> WString {
    let leading_slashes = path.chars().take_while(|c| *c == '/').count();
    let root_len = if allow_leading_double_slashes && leading_slashes == 2 {
        2
    } else {
        usize::from(leading_slashes > 0)
    };
    normalize_with_root(
        path,
        Root {
            prefix: path.slice_to(root_len),
            rooted: leading_slashes > 0,
        },
        |c| c == '/',
        '/',
    )
}

/// Wide character version of dirname().
#[cfg_attr(windows, allow(dead_code))]
pub fn wdirname(mut path: &wstr) -> &wstr {
    // Do not use system-provided dirname (#7837).
    // On Mac it's not thread safe, and will error for paths exceeding PATH_MAX.
    // This follows OpenGroup dirname recipe.

    // 1: Double-slash stays.
    if path == "//" {
        return path;
    }

    // 2: All slashes => return slash.
    if !path.is_empty() && path.chars().all(|c| c == '/') {
        return L!("/");
    }

    // 3: Trim trailing slashes.
    while path.as_char_slice().last() == Some(&'/') {
        path = path.slice_to(path.char_count() - 1);
    }

    // 4: No slashes left => return period.
    let Some(last_slash) = path.chars().rposition(|c| c == '/') else {
        return L!(".");
    };

    // 5: Remove trailing non-slashes.
    path = path.slice_to(last_slash + 1);

    // 6: Skip as permitted.
    // 7: Remove trailing slashes again.
    while path.as_char_slice().last() == Some(&'/') {
        path = path.slice_to(path.char_count() - 1);
    }

    // 8: Empty => return slash.
    if path.is_empty() {
        return L!("/");
    }
    path
}

/// Wide character version of basename().
#[cfg_attr(windows, allow(dead_code))]
pub fn wbasename(mut path: &wstr) -> &wstr {
    // This follows OpenGroup basename recipe.
    // 1: empty => allowed to return ".". This is what system impls do.
    if path.is_empty() {
        return L!(".");
    }

    // 2: Skip as permitted.
    // 3: All slashes => return slash.
    if path.chars().all(|c| c == '/') {
        return L!("/");
    }

    // 4: Remove trailing slashes.
    while path.as_char_slice().last() == Some(&'/') {
        path = path.slice_to(path.char_count() - 1);
    }

    // 5: Remove up to and including last slash.
    if let Some(last_slash) = path.chars().rposition(|c| c == '/') {
        path = path.slice_from(last_slash + 1);
    }
    path
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathError {
    Nul,
    DeviceNamespace,
    MalformedUnc,
}

#[cfg(any(windows, test))]
impl std::fmt::Display for PathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Nul => "path contains a NUL character",
            Self::DeviceNamespace => "Windows device and verbatim path namespaces are unsupported",
            Self::MalformedUnc => "UNC path requires a server and share name",
        })
    }
}

#[cfg(any(windows, test))]
impl std::error::Error for PathError {}

#[cfg(any(windows, test))]
fn windows_separator(c: char) -> bool {
    matches!(c, '/' | '\\')
}

#[cfg(any(windows, test))]
fn windows_root(path: &wstr) -> Result<Root<'_>, PathError> {
    let chars = path.as_char_slice();
    if chars.contains(&'\0') {
        return Err(PathError::Nul);
    }
    // Check the spelling directly: std::path can classify mixed-separator device prefixes as UNC.
    if chars.len() >= 4
        && windows_separator(chars[0])
        && windows_separator(chars[3])
        && ((windows_separator(chars[1]) && matches!(chars[2], '?' | '.'))
            || (chars[1] == '?' && chars[2] == '?'))
    {
        return Err(PathError::DeviceNamespace);
    }
    if chars.len() >= 2 && windows_separator(chars[0]) && windows_separator(chars[1]) {
        let server_end = chars[2..]
            .iter()
            .position(|c| windows_separator(*c))
            .map(|index| index + 2)
            .ok_or(PathError::MalformedUnc)?;
        let share_start = server_end + 1;
        let share_end = chars[share_start..]
            .iter()
            .position(|c| windows_separator(*c))
            .map_or(chars.len(), |index| index + share_start);
        let server = wstr::from_char_slice(&chars[2..server_end]);
        let share = wstr::from_char_slice(&chars[share_start..share_end]);
        if [server, share]
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        {
            return Err(PathError::MalformedUnc);
        }
        return Ok(Root {
            prefix: path.slice_to(share_end),
            rooted: true,
        });
    }
    if chars.len() >= 2 && chars[0].is_ascii_alphabetic() && chars[1] == ':' {
        let rooted = chars.get(2).is_some_and(|c| windows_separator(*c));
        return Ok(Root {
            prefix: path.slice_to(if rooted { 3 } else { 2 }),
            rooted,
        });
    }
    let rooted = chars.first().is_some_and(|c| windows_separator(*c));
    Ok(Root {
        prefix: path.slice_to(usize::from(rooted)),
        rooted,
    })
}

/// Reject unsupported Windows namespaces and malformed roots without reducing dot components.
#[cfg(any(windows, test))]
pub(crate) fn windows_validate(path: &wstr) -> Result<(), PathError> {
    windows_root(path).map(|_| ())
}

/// Normalize both separator spellings while preserving Windows root and drive-relative semantics.
#[cfg(any(windows, test))]
pub(crate) fn windows_normalize(path: &wstr) -> Result<WString, PathError> {
    let normalized = normalize_with_root(path, windows_root(path)?, windows_separator, '\\');
    // Dot-component reduction must not turn a root-relative path into a device namespace.
    windows_validate(&normalized)?;
    Ok(normalized)
}

#[cfg(any(windows, test))]
fn windows_trim_trailing_separators(path: &wstr, root_len: usize) -> &wstr {
    let chars = path.as_char_slice();
    let mut end = chars.len();
    while end > root_len && windows_separator(chars[end - 1]) {
        end -= 1;
    }
    path.slice_to(end)
}

/// Return a Windows parent directory, retaining the input spelling and its drive or UNC root.
#[cfg(any(windows, test))]
pub(crate) fn windows_dirname(path: &wstr) -> Result<WString, PathError> {
    let root = windows_root(path)?;
    let root_len = root.prefix.char_count();
    let path = windows_trim_trailing_separators(path, root_len);
    if let Some(last_separator) = path.as_char_slice()[root_len..]
        .iter()
        .rposition(|c| windows_separator(*c))
    {
        return Ok(windows_trim_trailing_separators(
            path.slice_to(root_len + last_separator),
            root_len,
        )
        .to_owned());
    }
    Ok(if root.prefix.is_empty() {
        L!(".").to_owned()
    } else {
        root.prefix.to_owned()
    })
}

/// Return a Windows basename without splitting a drive or UNC root into ordinary components.
#[cfg(any(windows, test))]
pub(crate) fn windows_basename(path: &wstr) -> Result<WString, PathError> {
    let root = windows_root(path)?;
    let root_len = root.prefix.char_count();
    let path = windows_trim_trailing_separators(path, root_len);
    if path.char_count() == root_len {
        return Ok(if root.rooted {
            root.prefix.to_owned()
        } else {
            L!(".").to_owned()
        });
    }
    let basename_start = path.as_char_slice()[root_len..]
        .iter()
        .rposition(|c| windows_separator(*c))
        .map_or(root_len, |index| root_len + index + 1);
    Ok(path.slice_from(basename_start).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fish_widestring::{ENCODE_DIRECT_BASE, str2wcstring};

    #[test]
    fn test_normalize_path() {
        fn norm_path(path: &wstr) -> WString {
            normalize_path(path, true)
        }
        assert_eq!(norm_path(L!("")), ".");
        assert_eq!(norm_path(L!("..")), "..");
        assert_eq!(norm_path(L!("./")), ".");
        assert_eq!(norm_path(L!("./.")), ".");
        assert_eq!(norm_path(L!("/")), "/");
        assert_eq!(norm_path(L!("//")), "//");
        assert_eq!(norm_path(L!("///")), "/");
        assert_eq!(norm_path(L!("////")), "/");
        assert_eq!(norm_path(L!("/.///")), "/");
        assert_eq!(norm_path(L!(".//")), ".");
        assert_eq!(norm_path(L!("/.//../")), "/");
        assert_eq!(norm_path(L!("////abc")), "/abc");
        assert_eq!(norm_path(L!("/abc")), "/abc");
        assert_eq!(norm_path(L!("/abc/")), "/abc");
        assert_eq!(norm_path(L!("/abc/..def/")), "/abc/..def");
        assert_eq!(norm_path(L!("//abc/../def/")), "//def");
        assert_eq!(norm_path(L!("abc/../abc/../abc/../abc")), "abc");
        assert_eq!(norm_path(L!("../../")), "../..");
        assert_eq!(norm_path(L!("foo/./bar")), "foo/bar");
        assert_eq!(norm_path(L!("foo/../")), ".");
        assert_eq!(norm_path(L!("foo/../foo")), "foo");
        assert_eq!(norm_path(L!("foo/../foo/")), "foo");
        assert_eq!(norm_path(L!("foo/././bar/.././baz")), "foo/baz");
        assert_eq!(normalize_path(L!("//abc/../def"), false), "/def");
        assert_eq!(norm_path(L!("foo\\bar/../baz")), "baz");
        assert_eq!(norm_path(L!("foo\\bar")), "foo\\bar");
    }

    #[test]
    fn preserves_wide_path_components() {
        let opaque = WString::from_chars([ENCODE_DIRECT_BASE, 'é', '雪']);
        let mut path = WString::from(L!("/"));
        path.push_utfstr(&opaque);
        path.push_str("/./leaf/..");
        let mut expected = WString::from(L!("/"));
        expected.push_utfstr(&opaque);
        assert_eq!(normalize_path(&path, true), expected);
        assert_eq!(wbasename(&expected), opaque);
        assert_eq!(wdirname(&expected), "/");
    }

    #[test]
    fn normalizes_windows_roots_and_components() {
        let cases = [
            ("", "."),
            (".", "."),
            (r".\", "."),
            (r"foo/./bar\\..\\baz", r"foo\baz"),
            (r"foo/../../bar", r"..\bar"),
            (r"../../", r"..\.."),
            ("C:", "C:."),
            (r"C:foo\..", "C:."),
            (r"C:foo\..\bar", "C:bar"),
            (r"c:../foo", r"c:..\foo"),
            (r"C:\", r"C:\"),
            (r"C:/", r"C:\"),
            (r"C:///Foo/../../bar", r"C:\bar"),
            (r"\", r"\"),
            (r"/", r"\"),
            (r"/foo/../../bar", r"\bar"),
            (r"\\Server\Share", r"\\Server\Share"),
            (r"//Server/Share/", r"\\Server\Share"),
            (r"\/Server/Share\foo/../bar", r"\\Server\Share\bar"),
            (r"\\Server\Share\..\..\bar", r"\\Server\Share\bar"),
            (r"\\Server\Share\foo\..", r"\\Server\Share"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                windows_normalize(&str2wcstring(input)).unwrap(),
                expected,
                "{input:?}"
            );
        }
    }

    #[test]
    fn splits_windows_paths_without_losing_roots() {
        let cases = [
            ("", ".", "."),
            ("foo", ".", "foo"),
            (r"foo\\", ".", "foo"),
            (r"foo/\\bar", "foo", "bar"),
            (r"foo\bar/baz", r"foo\bar", "baz"),
            ("C:", "C:", "."),
            ("C:foo", "C:", "foo"),
            (r"c:foo\bar", "c:foo", "bar"),
            (r"C:\", r"C:\", r"C:\"),
            (r"C:///", r"C:/", r"C:/"),
            (r"C:\foo\\", r"C:\", "foo"),
            (r"C:/foo/bar", "C:/foo", "bar"),
            (r"\", r"\", r"\"),
            ("/", "/", "/"),
            (r"\foo", r"\", "foo"),
            (r"\\Server\Share", r"\\Server\Share", r"\\Server\Share"),
            (r"//Server/Share/", "//Server/Share", "//Server/Share"),
            (r"\\Server\Share\foo", r"\\Server\Share", "foo"),
            (r"\\Server\Share\foo/bar", r"\\Server\Share\foo", "bar"),
        ];
        for (input, dir, base) in cases {
            let input = str2wcstring(input);
            assert_eq!(windows_dirname(&input).unwrap(), dir, "{input:?}");
            assert_eq!(windows_basename(&input).unwrap(), base, "{input:?}");
        }
    }

    #[test]
    fn rejects_windows_namespaces_and_malformed_unc_without_normalizing() {
        for path in [
            r"\\?\C:\file",
            r"//?/C:/file",
            r"\/?\C:/file",
            r"\\.\C:\file",
            r"//./pipe/name",
            r"\??\C:\file",
            r"/??/C:/file",
            r"\\?\UNC\server\share",
        ] {
            let path = str2wcstring(path);
            assert_eq!(windows_validate(&path), Err(PathError::DeviceNamespace));
            assert_eq!(windows_normalize(&path), Err(PathError::DeviceNamespace));
            assert_eq!(windows_dirname(&path), Err(PathError::DeviceNamespace));
            assert_eq!(windows_basename(&path), Err(PathError::DeviceNamespace));
        }
        for path in [
            r"\\",
            r"\\server",
            r"\\server\",
            r"\\server\\share",
            r"\\\server\share",
            r"\\server\.",
            r"\\server\..",
            r"\\..\share",
        ] {
            assert_eq!(
                windows_validate(&str2wcstring(path)),
                Err(PathError::MalformedUnc),
                "{path:?}"
            );
        }
        assert_eq!(windows_validate(L!("foo\0bar")), Err(PathError::Nul));
        assert_eq!(
            windows_normalize(&str2wcstring(r"\prefix\..\??\C:\file")),
            Err(PathError::DeviceNamespace)
        );
        // Validation leaves dot components for the filesystem adapter's resolution policy.
        assert_eq!(windows_validate(L!("C:/link/../file")), Ok(()));
        assert_eq!(windows_validate(L!("//server/share/link/../file")), Ok(()));
    }

    #[test]
    fn test_wdirname_wbasename() {
        // path, dir, base
        struct Test(&'static wstr, &'static wstr, &'static wstr);
        let testcases: &[Test] = &[
            Test(L!(""), L!("."), L!(".")),
            Test(L!("foo//"), L!("."), L!("foo")),
            Test(L!("foo//////"), L!("."), L!("foo")),
            Test(L!("/////foo"), L!("/"), L!("foo")),
            Test(L!("//foo/////bar"), L!("//foo"), L!("bar")),
            Test(L!("foo/////bar"), L!("foo"), L!("bar")),
            // Examples given in XPG4.2.
            Test(L!("/usr/lib"), L!("/usr"), L!("lib")),
            Test(L!("usr"), L!("."), L!("usr")),
            Test(L!("/"), L!("/"), L!("/")),
            Test(L!("."), L!("."), L!(".")),
            Test(L!(".."), L!("."), L!("..")),
        ];

        for tc in testcases {
            let Test(path, tc_dir, tc_base) = *tc;
            let dir = wdirname(path);
            assert_eq!(
                dir, tc_dir,
                "\npath: {:?}, dir: {:?}, tc.dir: {:?}",
                path, dir, tc_dir
            );

            let base = wbasename(path);
            assert_eq!(
                base, tc_base,
                "\npath: {:?}, base: {:?}, tc.base: {:?}",
                path, base, tc_base
            );
        }

        // Ensure strings which greatly exceed PATH_MAX still work (#7837).
        const LONG_PATH_LENGTH: usize = 8192;
        let mut longpath = WString::new();
        longpath.reserve(LONG_PATH_LENGTH + 10);
        while longpath.char_count() <= LONG_PATH_LENGTH {
            longpath.push_str("/overlong");
        }
        let last_slash = longpath.chars().rposition(|c| c == '/').unwrap();
        let longpath_dir = &longpath[..last_slash];
        assert_eq!(wdirname(&longpath), longpath_dir);
        assert_eq!(wbasename(&longpath), L!("overlong"));
    }
}
