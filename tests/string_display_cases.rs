// Terminal display behavior shared by the POSIX and native builtin suites.
pub fn check(mut validate: impl FnMut(&[&str], i32, &str)) {
    for (args, status, expected) in [
        (vec!["string", "pad"], 0, ""),
        (vec!["string", "pad", "-w4", "x"], 0, "   x\n"),
        (vec!["string", "pad", "-rw4", "x"], 0, "x   \n"),
        (vec!["string", "pad", "-Cw4", "x"], 0, "  x \n"),
        (vec!["string", "pad", "-rCw4", "x"], 0, " x  \n"),
        (vec!["string", "pad", "-Cw5", "x"], 0, "  x  \n"),
        (
            vec!["string", "pad", "--char=文", "--width=4", "x"],
            0,
            "文 x\n",
        ),
        (vec!["string", "pad", "-rc文", "-w4", "x"], 0, "x 文\n"),
        (
            vec!["string", "pad", "-w1", "a", "abcd", "文"],
            0,
            "   a\nabcd\n  文\n",
        ),
        (
            vec!["string", "pad", "-w4", "\x1b[31mred\x1b[m"],
            0,
            " \x1b[31mred\x1b[m\n",
        ),
        (vec!["string", "pad", "--char=xx", "x"], 2, ""),
        (vec!["string", "pad", "--char=\u{0301}", "x"], 2, ""),
        (vec!["string", "pad", "-w-1", "x"], 2, ""),
        (vec!["string", "length"], 1, ""),
        (vec!["string", "length", ""], 1, "0\n"),
        (vec!["string", "length", "文😀"], 0, "2\n"),
        (vec!["string", "length", "-V", "文😀"], 0, "4\n"),
        (vec!["string", "length", "-V", "a\u{0301}"], 0, "1\n"),
        (
            vec!["string", "length", "-V", "\x1b[31mred\x1b[m"],
            0,
            "3\n",
        ),
        (
            vec!["string", "length", "-V", "\x1b]0;title\x07x"],
            0,
            "1\n",
        ),
        (
            vec!["string", "length", "-V", "\x1b]0;title\x1b\\x"],
            0,
            "1\n",
        ),
        (
            vec!["string", "length", "-V", "\x1bktitle\x1b\\x"],
            0,
            "1\n",
        ),
        (
            vec!["string", "length", "-V", "abcdef\rxy\nz\r123"],
            0,
            "6\n3\n",
        ),
        (vec!["string", "length", "-V", "\x08abc\x08\x08"], 0, "1\n"),
        (vec!["string", "length", "-V", "a\x08\x08"], 1, "0\n"),
        (vec!["string", "length", "-qV", "", "x"], 0, ""),
        (
            vec!["string", "length", "--visible", "--quiet", "\x1b[m"],
            1,
            "",
        ),
        (vec!["string", "shorten", "-m3", "foo"], 1, "foo\n"),
        (vec!["string", "shorten", "-m2", "foo"], 0, "f…\n"),
        (vec!["string", "shorten", "-lm2", "foo"], 0, "…o\n"),
        (
            vec!["string", "shorten", "-m5", "--char=........", "foobar"],
            0,
            "fooba\n",
        ),
        (
            vec!["string", "shorten", "-lm5", "--char=........", "foobar"],
            0,
            "oobar\n",
        ),
        (
            vec!["string", "shorten", "--max=4", "-c///", "foobar"],
            0,
            "f///\n",
        ),
        (
            vec!["string", "shorten", "--max=2", "--chars=", "foo"],
            0,
            "fo\n",
        ),
        (vec!["string", "shorten", "foo", "foobar"], 0, "foo\nfo…\n"),
        (
            vec!["string", "shorten", "-c\x07", "foo", "foobar"],
            0,
            "foo\nfoo\x07\n",
        ),
        (
            vec!["string", "shorten", "-c\x08a", "foo", "foobar"],
            0,
            "foo\nfo\x08a\n",
        ),
        (vec!["string", "shorten", "-m2", "-q", "12"], 1, ""),
        (vec!["string", "shorten", "-lm2", "-q", "123"], 0, ""),
        (
            vec!["string", "shorten", "-qm0", "foo", "bar"],
            0,
            "foo\nbar\n",
        ),
        (vec!["string", "shorten", "-Ncx", "bar\nfooo"], 1, "barx\n"),
        (
            vec!["string", "shorten", "-Nlcx", "bar\nfooo"],
            1,
            "fooox\n",
        ),
        (vec!["string", "shorten", "-Ncx", "bar"], 1, "barx\n"),
        (
            vec!["string", "shorten", "-m4", "foobar\nbananarama"],
            0,
            "foo…\nban…\n",
        ),
        (
            vec!["string", "shorten", "-lm4", "foobar\nbananarama"],
            0,
            "…bar\n…ama\n",
        ),
        (
            vec!["string", "shorten", "-m4", "\x1b[31mfoobar\x1b[m"],
            0,
            "\x1b[31mfoo…\n",
        ),
        (
            vec!["string", "shorten", "-lm4", "\x1b[31mfoobar\x1b[m"],
            0,
            "…bar\x1b[m\n",
        ),
        (
            vec!["string", "shorten", "-m10", "\x1b[31mfoo\x1b[m"],
            1,
            "\x1b[31mfoo\x1b[m\n",
        ),
        (
            vec!["string", "shorten", "-m4", "a\u{0301}bcde"],
            0,
            "a\u{0301}bc…\n",
        ),
        (
            vec![
                "string",
                "shorten",
                "abc",
                "ab",
                "abcdef\x08\x08\x08\x08\x08\x08\x08",
            ],
            0,
            "a…\nab\nabcdef\x08\x08\x08\x08\x08\x08\x08\n",
        ),
        (
            vec!["string", "shorten", "abc", "\x08abc", "ab"],
            0,
            "a…\n\x08a…\nab\n",
        ),
        (
            vec!["string", "shorten", "-lm3", "abcde\x08x"],
            0,
            "…de\x08x\n",
        ),
        (vec!["string", "shorten", "--max=-1", "foo"], 2, ""),
    ] {
        validate(&args, status, expected);
    }
}
