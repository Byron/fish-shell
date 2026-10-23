// Copyright (C) 1999, 2000 Free Software Foundation, Inc.
// Rust translation: Copyright (C) 2026 fish-shell contributors.
//
// This file is derived from GNU Bash 3.2, lib/sh/shquote.c and syntax.h.
// You may redistribute and/or modify it under the GNU General Public License,
// version 2 or (at your option) any later version. It comes WITHOUT ANY WARRANTY;
// without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See ../COPYING for the full license.

//! Bash's quoting helpers, including its internal expansion markers.
//!
//! These are distinct operations from `lib/sh/shquote.c`, not interchangeable
//! ways of escaping arbitrary shell input. In particular, double quoting does
//! not protect interactive history expansion. Backslash quoting leaves tilde
//! active and emits backslash-newline, which the parser removes rather than
//! preserving as a literal newline. Input is a C string because Bash words cannot
//! contain NUL; every other byte, including invalid UTF-8, is preserved.

use std::ffi::{CStr, CString};

const CTLESC: u8 = 1;
const CTLNUL: u8 = 127;
const METACHARACTERS: &[u8] = b" \t\n'\"\\|&;()<>!{}*[]?^$`";

// CBSDQUOTE from Bash 3.2's syntax.h/mksyntax.c.
fn double_quote_special(c: u8) -> bool {
    matches!(c, b'\\' | b'`' | b'$' | b'"' | b'\n')
}

fn string(bytes: Vec<u8>) -> CString {
    CString::new(bytes).expect("quoting a C string cannot introduce NUL")
}

/// `sh_single_quote`: a complete single-quoted word, used by `alias` and `trap`.
pub fn single_quote(input: &CStr) -> CString {
    let mut result = vec![b'\''];
    for &c in input.to_bytes() {
        result.push(c);
        if c == b'\'' {
            result.extend_from_slice(b"\\''");
        }
    }
    result.push(b'\'');
    string(result)
}

/// `sh_double_quote`: a double-quoted word for Bash's expansion machinery.
///
/// Internal CTLESC/CTLNUL bytes acquire a CTLESC prefix. The result is therefore
/// not a general-purpose literal for submission to a shell parser.
pub fn double_quote(input: &CStr) -> CString {
    let mut result = vec![b'"'];
    for &c in input.to_bytes() {
        if double_quote_special(c) && c != b'\n' {
            result.push(b'\\');
        } else if matches!(c, CTLESC | CTLNUL) {
            result.push(CTLESC);
        }
        result.push(c);
    }
    result.push(b'"');
    string(result)
}

/// `sh_mkdoublequoted`: surround input with quotes, optionally escaping `"`.
/// Other expansion characters remain active.
pub fn make_double_quoted(input: &CStr, escape_quotes: bool) -> CString {
    let mut result = vec![b'"'];
    for &c in input.to_bytes() {
        if escape_quotes && c == b'"' {
            result.push(b'\\');
        }
        result.push(c);
    }
    result.push(b'"');
    string(result)
}

/// `sh_un_double_quote`: remove backslashes before CBSDQUOTE characters.
/// This helper retains an escaped newline and does not remove surrounding quotes.
pub fn un_double_quote(input: &CStr) -> CString {
    let mut result = Vec::new();
    let mut bytes = input.to_bytes().iter().copied().peekable();
    while let Some(c) = bytes.next() {
        if c == b'\\' && bytes.peek().is_some_and(|c| double_quote_special(*c)) {
            result.push(bytes.next().unwrap());
            continue;
        }
        result.push(c);
    }
    string(result)
}

/// `sh_backslash_quote`: quote the metacharacters selected by Bash 3.2.
/// A leading `#` is quoted; subsequent `#` bytes and all tildes are preserved.
pub fn backslash_quote(input: &CStr) -> CString {
    let mut result = Vec::new();
    for (i, &c) in input.to_bytes().iter().enumerate() {
        if METACHARACTERS.contains(&c) || c == b',' || c == b'#' && i == 0 {
            result.push(b'\\');
        }
        result.push(c);
    }
    string(result)
}

/// `sh_backslash_quote_for_double_quotes`, used by prompt expansion.
/// Unlike `double_quote`, this prefixes newlines with a backslash.
pub fn backslash_quote_for_double_quotes(input: &CStr) -> CString {
    let mut result = Vec::new();
    for &c in input.to_bytes() {
        if double_quote_special(c) {
            result.push(b'\\');
        } else if matches!(c, CTLESC | CTLNUL) {
            result.push(CTLESC);
        }
        result.push(c);
    }
    string(result)
}

/// `sh_contains_shell_metas`: Bash's context-sensitive metacharacter predicate.
/// This is not a parser or an assertion that a word is safe to evaluate.
pub fn contains_shell_metas(input: &CStr) -> bool {
    let bytes = input.to_bytes();
    bytes.iter().enumerate().any(|(i, &c)| {
        METACHARACTERS.contains(&c)
            || c == b'~' && (i == 0 || matches!(bytes[i - 1], b'=' | b':'))
            || c == b'#' && i == 0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_preserve_bytes_and_internal_markers() {
        assert_eq!(single_quote(c"").as_c_str(), c"''");
        assert_eq!(single_quote(c"a'b").as_c_str(), c"'a'\\''b'");
        assert_eq!(single_quote(c"raw-\xff\n").as_c_str(), c"'raw-\xff\n'");
        assert_eq!(
            double_quote(c"$`\"\\\n\x01\x7f\xff").as_c_str(),
            c"\"\\$\\`\\\"\\\\\n\x01\x01\x01\x7f\xff\""
        );
        assert_eq!(
            backslash_quote_for_double_quotes(c"$\n\x01\x7f").as_c_str(),
            c"\\$\\\n\x01\x01\x01\x7f"
        );
        assert_eq!(make_double_quoted(c"a\"$b", false).as_c_str(), c"\"a\"$b\"");
        assert_eq!(
            make_double_quoted(c"a\"$b", true).as_c_str(),
            c"\"a\\\"$b\""
        );
    }

    #[test]
    fn undouble_quoting_does_not_unescape_twice() {
        assert_eq!(
            un_double_quote(c"\\$\\`\\\"\\\\\\q\\").as_c_str(),
            c"$`\"\\\\q\\"
        );
        assert_eq!(un_double_quote(c"\\\\$").as_c_str(), c"\\$");
        assert_eq!(un_double_quote(c"\\\n").as_c_str(), c"\n");
    }

    #[test]
    fn backslash_quoting_keeps_the_baseline_limitations() {
        assert_eq!(
            backslash_quote(c"#a #b ~\n,").as_c_str(),
            c"\\#a\\ #b\\ ~\\\n\\,"
        );
        for s in [c"", c"a,b", c"foo#bar", c"foo~bar", c"raw-\xff"] {
            assert!(!contains_shell_metas(s), "{s:?}");
        }
        for s in [c"#comment", c"~/a", c"x=~", c"x:~", c"a b", c"$x", c"[]"] {
            assert!(contains_shell_metas(s), "{s:?}");
        }
    }
}
