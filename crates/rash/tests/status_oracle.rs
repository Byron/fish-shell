//! Explicit differential checks against the selected Bash 3.2 oracle.

use rash::status::{ProcessStatus, invert_status, pipeline_status};
use std::env;
use std::path::Path;
use std::process::{Command, Output};

fn bash(oracle: &Path, source: &str) -> Output {
    Command::new(oracle)
        .args(["--noprofile", "--norc", "-c", source])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .output()
        .expect("run BASH_ORACLE")
}

fn output_text(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).expect("oracle output is ASCII")
}

#[test]
#[ignore = "requires BASH_ORACLE pointing to GNU Bash 3.2.57"]
fn pipeline_status_matches_bash() {
    let oracle = env::var_os("BASH_ORACLE").expect("set BASH_ORACLE to GNU Bash 3.2.57");
    let oracle = Path::new(&oracle);
    let version = output_text(bash(oracle, "printf '%s' \"$BASH_VERSION\""));
    assert!(
        version.starts_with("3.2.57("),
        "wrong Bash oracle: {version}"
    );

    let mut cases = Vec::new();
    for a in [0, 1, 7, 255] {
        cases.push(vec![(format!("(exit {a})"), ProcessStatus::Exited(a))]);
        for b in [0, 1, 7, 255] {
            for c in [0, 1, 7, 255] {
                cases.push(
                    [a, b, c]
                        .into_iter()
                        .map(|code| (format!("(exit {code})"), ProcessStatus::Exited(code)))
                        .collect(),
                );
            }
        }
    }

    // Query signal numbers from this platform instead of assuming Linux numbering.
    for name in ["TERM", "PIPE", "KILL"] {
        let signal: i32 = output_text(bash(oracle, &format!("kill -l {name}")))
            .trim()
            .parse()
            .expect("numeric signal");
        let killed = (
            format!("/bin/sh -c 'kill -{name} $$'"),
            ProcessStatus::Signaled(signal),
        );
        cases.push(vec![killed.clone()]);
        for position in 0..3 {
            let mut stages = vec![
                ("(exit 7)".to_owned(), ProcessStatus::Exited(7)),
                ("(exit 3)".to_owned(), ProcessStatus::Exited(3)),
                ("(exit 0)".to_owned(), ProcessStatus::Exited(0)),
            ];
            stages[position] = killed.clone();
            cases.push(stages);
        }
    }

    let mut source = String::new();
    let mut expected = Vec::new();
    for stages in cases {
        let commands = stages
            .iter()
            .map(|(command, _)| command.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        let outcomes = stages.iter().map(|(_, status)| *status).collect::<Vec<_>>();
        for pipefail in [false, true] {
            for invert in [false, true] {
                source.push_str(if pipefail {
                    "set -o pipefail\n"
                } else {
                    "set +o pipefail\n"
                });
                if invert {
                    source.push_str("! ");
                }
                source.push_str(&commands);
                // Expand $? and PIPESTATUS in the same command, before either changes.
                source.push_str("\nprintf '%s ' \"$?\" \"${PIPESTATUS[@]}\"; printf '\\n'\n");

                let selected = pipeline_status(&outcomes, pipefail).unwrap();
                let status = if invert {
                    invert_status(selected.exit_status())
                } else {
                    selected.exit_status()
                };
                let row = std::iter::once(status)
                    .chain(outcomes.iter().map(|stage| stage.exit_status()))
                    .collect::<Vec<_>>();
                expected.push((commands.clone(), pipefail, invert, row));
            }
        }
    }

    let output = output_text(bash(oracle, &source));
    let actual = output.lines().collect::<Vec<_>>();
    assert_eq!(actual.len(), expected.len(), "oracle output:\n{output}");
    for (actual, (commands, pipefail, invert, expected)) in actual.iter().zip(expected) {
        let actual = actual
            .split_whitespace()
            .map(|value| value.parse::<i32>().expect("numeric status"))
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected,
            "{commands}; pipefail={pipefail}, invert={invert}"
        );
    }
}
