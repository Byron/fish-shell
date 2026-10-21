//! Compare all strtrans.c helpers against unchanged Bash 3.2 C in a test process.

use rash::ansi_c;
use std::ffi::CString;
use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command, Stdio};

fn printable(byte: u8) -> bool {
    (b' '..=b'~').contains(&byte)
}

fn upper(byte: u8) -> u8 {
    byte.to_ascii_uppercase()
}

fn record(bytes: Option<&[u8]>, saw_c: bool) -> String {
    let Some(bytes) = bytes else {
        return "null".to_owned();
    };
    let mut result = format!("{}:", u8::from(saw_c));
    for byte in bytes {
        write!(result, "{byte:02x}").unwrap();
    }
    result
}

#[test]
#[ignore = "requires RASH_ANSI_C_ORACLE built by build_tools/rash/build-ansi-c-oracle.py"]
fn ansi_c_matches_bash_source() {
    let oracle = std::env::var_os("RASH_ANSI_C_ORACLE").expect("set RASH_ANSI_C_ORACLE");
    let mut cases = vec![
        CString::default(),
        CString::new((1..=255).collect::<Vec<u8>>()).unwrap(),
    ];
    for byte in 1..=255 {
        cases.push(CString::new(vec![byte]).unwrap());
        cases.push(CString::new(vec![b'\\', byte]).unwrap());
        cases.push(CString::new(vec![b'\\', b'c', byte]).unwrap());
    }
    for a in b'0'..=b'9' {
        for b in b'0'..=b'9' {
            cases.push(CString::new(vec![b'\\', a, b, b'7', b'7']).unwrap());
            cases.push(CString::new(vec![b'\\', b'x', a, b, b'F']).unwrap());
        }
    }
    for case in [
        c"plain and ü🐟",
        c"\\a\\v\\b\\e\\E\\f\\n\\r\\t",
        c"\\'\\\"\\?\\\\\\q\\",
        c"\\0a\\000b\\x00c\\0000",
        c"\\x\\x!\\x{1234}\\x{}\\x{!}\\x{ab!",
        c"\\x{123456}\\x{ff}\\x{7f}\\x{1}",
        c"\\u0041\\U0001F41F",
        c"before\\cafter\\nignored",
        c"\\c",
        c"\\c\\",
        c"\x01\\001\x7f\\177\xff",
        c"prefix\\001suffix",
    ] {
        cases.push(case.to_owned());
    }
    let mut input = Vec::new();
    for case in &cases {
        input.extend_from_slice(case.to_bytes_with_nul());
    }
    let mut operations = vec![
        vec!["quote".to_owned()],
        vec!["should-quote".to_owned()],
        vec!["expand".to_owned()],
        vec!["expand-inner".to_owned()],
    ];
    for flags in 0..8 {
        for stop_at_c in [false, true] {
            operations.push(vec![
                "decode".to_owned(),
                flags.to_string(),
                u8::from(stop_at_c).to_string(),
            ]);
        }
    }
    for args in operations {
        let mut child = Command::new(&oracle)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run ANSI-C oracle");
        let mut stdin = child.stdin.take().unwrap();
        let input = input.clone();
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output().unwrap();
        writer.join().unwrap().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let output = String::from_utf8(output.stdout).unwrap();
        let mut actual = output.lines();
        for case in &cases {
            let expected = match args[0].as_str() {
                "quote" => record(
                    ansi_c::quote(case, printable)
                        .as_deref()
                        .map(|s| s.to_bytes()),
                    false,
                ),
                "should-quote" => record(
                    Some(&[u8::from(ansi_c::should_quote(case, printable))]),
                    false,
                ),
                "expand" | "expand-inner" => {
                    let len = case.to_bytes().len();
                    let range = if args[0] == "expand" {
                        0..len
                    } else if len >= 2 {
                        1..len - 1
                    } else {
                        0..0
                    };
                    record(ansi_c::expand_range(case, range, upper).as_deref(), false)
                }
                "decode" => {
                    let result =
                        ansi_c::decode(case, args[1].parse().unwrap(), args[2] == "1", upper);
                    record(
                        result.as_ref().map(|r| r.bytes.as_slice()),
                        result.as_ref().is_some_and(|r| r.saw_c),
                    )
                }
                _ => unreachable!(),
            };
            assert_eq!(actual.next(), Some(expected.as_str()), "{args:?}: {case:?}");
        }
        assert!(actual.next().is_none(), "extra oracle output: {args:?}");
    }
}
