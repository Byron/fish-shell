//! Compare every quoting helper with unchanged Bash 3.2 C, in a test-only process.

use rash::quote;
use std::ffi::{CStr, CString};
use std::io::Write as _;
use std::process::{Command, Stdio};

#[test]
#[ignore = "requires RASH_QUOTE_ORACLE built by build_tools/rash/build-quote-oracle.py"]
fn quoting_matches_bash_source() {
    let oracle = std::env::var_os("RASH_QUOTE_ORACLE").expect("set RASH_QUOTE_ORACLE");
    let mut cases = vec![
        CString::default(),
        CString::new((1..=255).collect::<Vec<u8>>()).unwrap(),
    ];
    for a in 1..=255 {
        cases.push(CString::new(vec![a]).unwrap());
        for b in 1..=255 {
            cases.push(CString::new(vec![a, b]).unwrap());
        }
    }
    for case in [
        c"'quotes' \"and\" $expansions",
        c"\\\\$\\\\`\\\\\n",
        c"x=~:~ ~/x #mid",
        c"ü🐟",
    ] {
        cases.push(case.to_owned());
    }
    let mut input = Vec::new();
    for case in &cases {
        input.extend_from_slice(case.to_bytes_with_nul());
    }

    type Quoter = fn(&CStr) -> CString;
    let operations: [(&str, Quoter); 8] = [
        ("single", quote::single_quote),
        ("double", quote::double_quote),
        ("make-double", |s| quote::make_double_quoted(s, false)),
        ("make-double-escaped", |s| {
            quote::make_double_quoted(s, true)
        }),
        ("un-double", quote::un_double_quote),
        ("backslash", quote::backslash_quote),
        ("prompt", quote::backslash_quote_for_double_quotes),
        ("metas", |s| {
            if quote::contains_shell_metas(s) {
                c"1"
            } else {
                c"0"
            }
            .to_owned()
        }),
    ];
    for (name, operation) in operations {
        let mut child = Command::new(&oracle)
            .arg(name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run quoting oracle");
        let mut stdin = child.stdin.take().unwrap();
        let input = input.clone();
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output().unwrap();
        writer.join().unwrap().unwrap();
        assert!(output.status.success(), "{name}: {output:?}");
        let mut actual = output.stdout.split(|&b| b == 0);
        for case in &cases {
            assert_eq!(
                actual.next(),
                Some(operation(case).to_bytes()),
                "{name}: {case:?}"
            );
        }
        assert_eq!(actual.next(), Some([].as_slice()));
        assert!(actual.next().is_none());
    }
}
