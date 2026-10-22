//! Established POSIX string cases shared with native integration tests.
const STATUS_CMD_OK: i32 = 0;
const STATUS_CMD_ERROR: i32 = 1;
const STATUS_INVALID_ARGS: i32 = 2;

#[rustfmt::skip]
pub fn match_cases(mut check: impl FnMut(&[&str], i32, &str)) {
    macro_rules! validate {
        ([$($arg:expr),*], $status:expr, $output:expr) => { check(&[$($arg),*], $status, $output) };
    }
        validate!(["string", "match"], STATUS_INVALID_ARGS, "");
        validate!(["string", "match", ""], STATUS_CMD_ERROR, "");
        validate!(["string", "match", "", ""], STATUS_CMD_OK, "\n");
        validate!(["string", "match", "*", ""], STATUS_CMD_OK, "\n");
        validate!(["string", "match", "**", ""], STATUS_CMD_OK, "\n");
        validate!(["string", "match", "*", "xyzzy"], STATUS_CMD_OK, "xyzzy\n");
        validate!(["string", "match", "**", "plugh"], STATUS_CMD_OK, "plugh\n");
        validate!(["string", "match", "a*b", "axxb"], STATUS_CMD_OK, "axxb\n");
        validate!(["string", "match", "a*", "axxb"], STATUS_CMD_OK, "axxb\n");
        validate!(["string", "match", "*a", "xxa"], STATUS_CMD_OK, "xxa\n");
        validate!(["string", "match", "*a*", "axa"], STATUS_CMD_OK, "axa\n");
        validate!(["string", "match", "*a*", "xax"], STATUS_CMD_OK, "xax\n");
        validate!(["string", "match", "*a*", "bxa"], STATUS_CMD_OK, "bxa\n");
        validate!(["string", "match", "*a", "a"], STATUS_CMD_OK, "a\n");
        validate!(["string", "match", "a*", "a"], STATUS_CMD_OK, "a\n");
        validate!(["string", "match", "a*b*c", "axxbyyc"], STATUS_CMD_OK, "axxbyyc\n");
        validate!(["string", "match", "\\*", "*"], STATUS_CMD_OK, "*\n");
        validate!(["string", "match", "a*\\", "abc\\"], STATUS_CMD_OK, "abc\\\n");

        validate!(["string", "match", "a*b", "axxbc"], STATUS_CMD_ERROR, "");
        validate!(["string", "match", "*b", "bbba"], STATUS_CMD_ERROR, "");
        validate!(["string", "match", "0x[0-9a-fA-F][0-9a-fA-F]", "0xbad"], STATUS_CMD_ERROR, "");

        validate!(["string", "match", "-a", "*", "ab", "cde"], STATUS_CMD_OK, "ab\ncde\n");
        validate!(["string", "match", "*", "ab", "cde"], STATUS_CMD_OK, "ab\ncde\n");
        validate!(["string", "match", "-n", "*d*", "cde"], STATUS_CMD_OK, "1 3\n");
        validate!(["string", "match", "-n", "*x*", "cde"], STATUS_CMD_ERROR, "");
        validate!(["string", "match", "-q", "a*", "b", "c"], STATUS_CMD_ERROR, "");
        validate!(["string", "match", "-q", "a*", "b", "a"], STATUS_CMD_OK, "");

        validate!(["string", "match", "-r"], STATUS_INVALID_ARGS, "");
        validate!(["string", "match", "-r", ""], STATUS_CMD_ERROR, "");
        validate!(["string", "match", "-r", "", ""], STATUS_CMD_OK, "\n");
        validate!(["string", "match", "-r", ".", "a"], STATUS_CMD_OK, "a\n");
        validate!(["string", "match", "-r", ".*", ""], STATUS_CMD_OK, "\n");
        validate!(["string", "match", "-r", "a*b", "b"], STATUS_CMD_OK, "b\n");
        validate!(["string", "match", "-r", "a*b", "aab"], STATUS_CMD_OK, "aab\n");
        validate!(["string", "match", "-r", "-i", "a*b", "Aab"], STATUS_CMD_OK, "Aab\n");
        validate!(["string", "match", "-r", "-a", "a[bc]", "abadac"], STATUS_CMD_OK, "ab\nac\n");
        validate!(["string", "match", "-r", "a", "xaxa", "axax"], STATUS_CMD_OK, "a\na\n");
        validate!(["string", "match", "-r", "-a", "a", "xaxa", "axax"], STATUS_CMD_OK, "a\na\na\na\n");
        validate!(["string", "match", "-r", "a[bc]", "abadac"], STATUS_CMD_OK, "ab\n");
        validate!(["string", "match", "-r", "-q", "a[bc]", "abadac"], STATUS_CMD_OK, "");
        validate!(["string", "match", "-r", "-q", "a[bc]", "ad"], STATUS_CMD_ERROR, "");
        validate!(["string", "match", "-r", "(a+)b(c)", "aabc"], STATUS_CMD_OK, "aabc\naa\nc\n");
        validate!(["string", "match", "-r", "-a", "(a)b(c)", "abcabc"], STATUS_CMD_OK, "abc\na\nc\nabc\na\nc\n");
        validate!(["string", "match", "-r", "(a)b(c)", "abcabc"], STATUS_CMD_OK, "abc\na\nc\n");
        validate!(["string", "match", "-r", "(a|(z))(bc)", "abc"], STATUS_CMD_OK, "abc\na\nbc\n");
        validate!(["string", "match", "-r", "-n", "a", "ada", "dad"], STATUS_CMD_OK, "1 1\n2 1\n");
        validate!(["string", "match", "-r", "-n", "-a", "a", "bacadae"], STATUS_CMD_OK, "2 1\n4 1\n6 1\n");
        validate!(["string", "match", "-r", "-n", "(a).*(b)", "a---b"], STATUS_CMD_OK, "1 5\n1 1\n5 1\n");
        validate!(["string", "match", "-r", "-n", "(a)(b)", "ab"], STATUS_CMD_OK, "1 2\n1 1\n2 1\n");
        validate!(["string", "match", "-r", "-n", "(a)(b)", "abab"], STATUS_CMD_OK, "1 2\n1 1\n2 1\n");
        validate!(["string", "match", "-r", "-n", "-a", "(a)(b)", "abab"], STATUS_CMD_OK, "1 2\n1 1\n2 1\n3 2\n3 1\n4 1\n");
        validate!(["string", "match", "-r", "*", ""], STATUS_INVALID_ARGS, "");
        validate!(["string", "match", "-r", "-a", "a*", "b"], STATUS_CMD_OK, "\n\n");
        validate!(["string", "match", "-r", "foo\\Kbar", "foobar"], STATUS_CMD_OK, "bar\n");
        validate!(["string", "match", "-r", "(foo)\\Kbar", "foobar"], STATUS_CMD_OK, "bar\nfoo\n");
}

#[rustfmt::skip]
pub fn replace_cases(mut check: impl FnMut(&[&str], i32, &str)) {
    macro_rules! validate {
        ([$($arg:expr),*], $status:expr, $output:expr) => { check(&[$($arg),*], $status, $output) };
    }
        validate!(["string", "replace", ""], STATUS_INVALID_ARGS, "");
        validate!(["string", "replace", "", ""], STATUS_CMD_ERROR, "");
        validate!(["string", "replace", "", "", ""], STATUS_CMD_ERROR, "\n");
        validate!(["string", "replace", "", "", " "], STATUS_CMD_ERROR, " \n");
        validate!(["string", "replace", "a", "b", ""], STATUS_CMD_ERROR, "\n");
        validate!(["string", "replace", "a", "b", "a"], STATUS_CMD_OK, "b\n");
        validate!(["string", "replace", "a", "b", "xax"], STATUS_CMD_OK, "xbx\n");
        validate!(["string", "replace", "a", "b", "xax", "axa"], STATUS_CMD_OK, "xbx\nbxa\n");
        validate!(["string", "replace", "bar", "x", "red barn"], STATUS_CMD_OK, "red xn\n");
        validate!(["string", "replace", "x", "bar", "red xn"], STATUS_CMD_OK, "red barn\n");
        validate!(["string", "replace", "--", "x", "-", "xyz"], STATUS_CMD_OK, "-yz\n");
        validate!(["string", "replace", "--", "y", "-", "xyz"], STATUS_CMD_OK, "x-z\n");
        validate!(["string", "replace", "--", "z", "-", "xyz"], STATUS_CMD_OK, "xy-\n");
        validate!(["string", "replace", "-i", "z", "X", "_Z_"], STATUS_CMD_OK, "_X_\n");
        validate!(["string", "replace", "-a", "a", "A", "aaa"], STATUS_CMD_OK, "AAA\n");
        validate!(["string", "replace", "-i", "a", "z", "AAA"], STATUS_CMD_OK, "zAA\n");
        validate!(["string", "replace", "-q", "x", ">x<", "x"], STATUS_CMD_OK, "");
        validate!(["string", "replace", "-a", "x", "", "xxx"], STATUS_CMD_OK, "\n");
        validate!(["string", "replace", "-a", "***", "_", "*****"], STATUS_CMD_OK, "_**\n");
        validate!(["string", "replace", "-a", "***", "***", "******"], STATUS_CMD_OK, "******\n");
        validate!(["string", "replace", "-a", "a", "b", "xax", "axa"], STATUS_CMD_OK, "xbx\nbxb\n");

        validate!(["string", "replace", "-r"], STATUS_INVALID_ARGS, "");
        validate!(["string", "replace", "-r", ""], STATUS_INVALID_ARGS, "");
        validate!(["string", "replace", "-r", "", ""], STATUS_CMD_ERROR, "");
        validate!(["string", "replace", "-r", "", "", ""], STATUS_CMD_OK, "\n");  // pcre2 behavior
        validate!(["string", "replace", "-r", "", "", " "], STATUS_CMD_OK, " \n");  // pcre2 behavior
        validate!(["string", "replace", "-r", "a", "b", ""], STATUS_CMD_ERROR, "\n");
        validate!(["string", "replace", "-r", "a", "b", "a"], STATUS_CMD_OK, "b\n");
        validate!(["string", "replace", "-r", ".", "x", "abc"], STATUS_CMD_OK, "xbc\n");
        validate!(["string", "replace", "-r", ".", "", "abc"], STATUS_CMD_OK, "bc\n");
        validate!(["string", "replace", "-r", "(\\w)(\\w)", "$2$1", "ab"], STATUS_CMD_OK, "ba\n");
        validate!(["string", "replace", "-r", "(\\w)", "$1$1", "ab"], STATUS_CMD_OK, "aab\n");
        validate!(["string", "replace", "-r", "-a", ".", "x", "abc"], STATUS_CMD_OK, "xxx\n");
        validate!(["string", "replace", "-r", "-a", "(\\w)", "$1$1", "ab"], STATUS_CMD_OK, "aabb\n");
        validate!(["string", "replace", "-r", "-a", ".", "", "abc"], STATUS_CMD_OK, "\n");
        validate!(["string", "replace", "-r", "a", "x", "bc", "cd", "de"], STATUS_CMD_ERROR, "bc\ncd\nde\n");
        validate!(["string", "replace", "-r", "a", "x", "aba", "caa"], STATUS_CMD_OK, "xba\ncxa\n");
        validate!(["string", "replace", "-r", "-a", "a", "x", "aba", "caa"], STATUS_CMD_OK, "xbx\ncxx\n");
        validate!(["string", "replace", "-r", "-i", "A", "b", "xax"], STATUS_CMD_OK, "xbx\n");
        validate!(["string", "replace", "-r", "-i", "[a-z]", ".", "1A2B"], STATUS_CMD_OK, "1.2B\n");
        validate!(["string", "replace", "-r", "A", "b", "xax"], STATUS_CMD_ERROR, "xax\n");
        validate!(["string", "replace", "-r", "a", "$1", "a"], STATUS_INVALID_ARGS, "");
        validate!(["string", "replace", "-r", "(a)", "$2", "a"], STATUS_INVALID_ARGS, "");
        validate!(["string", "replace", "-r", "*", ".", "a"], STATUS_INVALID_ARGS, "");
        validate!(["string", "replace", "-ra", "x", "\\c"], STATUS_CMD_ERROR, "");
        validate!(["string", "replace", "-r", "^(.)", "\t$1", "abc", "x"], STATUS_CMD_OK, "\tabc\n\tx\n");
}

#[rustfmt::skip]
pub fn escape_cases(mut check: impl FnMut(&[&str], i32, &str)) {
    macro_rules! validate {
        ([$($arg:expr),*], $status:expr, $output:expr) => { check(&[$($arg),*], $status, $output) };
    }
        validate!(["string", "escape"], STATUS_CMD_ERROR, "");
        validate!(["string", "escape", ""], STATUS_CMD_OK, "''\n");
        validate!(["string", "escape", "-n", ""], STATUS_CMD_OK, "\n");
        validate!(["string", "escape", "a"], STATUS_CMD_OK, "a\n");
        validate!(["string", "escape", "\x07"], STATUS_CMD_OK, "\\cg\n");
        validate!(["string", "escape", "\"x\""], STATUS_CMD_OK, "'\"x\"'\n");
        validate!(["string", "escape", "hello world"], STATUS_CMD_OK, "'hello world'\n");
        validate!(["string", "escape", "-n", "hello world"], STATUS_CMD_OK, "hello\\ world\n");
        validate!(["string", "escape", "hello", "world"], STATUS_CMD_OK, "hello\nworld\n");
        validate!(["string", "escape", "-n", "~"], STATUS_CMD_OK, "\\~\n");
}
