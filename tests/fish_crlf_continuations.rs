//! LF and CRLF continuations have the same script semantics on both backends.
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fish-crlf-{}-{} space",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn check(&self, source: &str, expected: &[u8]) {
        let script = self.0.join("script.fish");
        std::fs::write(&script, source.as_bytes()).unwrap();
        for file_input in [false, true] {
            for syntax_only in [false, true] {
                let mut command = Command::new(env!("CARGO_BIN_EXE_fish"));
                command.env_clear();
                if let Some(system_root) = std::env::var_os("SystemRoot") {
                    command.env("SystemRoot", system_root);
                }
                command
                    .current_dir(&self.0)
                    .env("HOME", &self.0)
                    .env("USERPROFILE", &self.0)
                    .env("FISH_NATIVE_CONFIG_HOME", self.0.join("config"))
                    .env("XDG_CONFIG_HOME", self.0.join("config"))
                    .env("XDG_DATA_HOME", self.0.join("data"))
                    .env("XDG_CACHE_HOME", self.0.join("cache"))
                    .env("XDG_DATA_DIRS", self.0.join("vendor"))
                    .env("PATH", "")
                    .env("TEMP", &self.0)
                    .env("TMP", &self.0)
                    .arg("-N")
                    .stdin(Stdio::null());
                if syntax_only {
                    command.arg("-n");
                }
                if file_input {
                    command.arg(&script);
                } else {
                    command.args(["-c", source]);
                }
                let result = command.output().unwrap();
                assert!(
                    result.status.success() && result.stderr.is_empty(),
                    "file={file_input} syntax={syntax_only} source={source:?}: {result:?}"
                );
                assert_eq!(
                    result.stdout,
                    if syntax_only { b"" } else { expected },
                    "file={file_input} syntax={syntax_only} source={source:?}"
                );
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let (Ok(root), Ok(temp)) = (self.0.canonicalize(), std::env::temp_dir().canonicalize())
        else {
            return;
        };
        if root.parent() == Some(temp.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fish-crlf-"))
        {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[test]
fn continued_arguments_pipelines_and_comments_use_the_complete_line_ending() {
    let fixture = Fixture::new();
    let source = "function joined\n    printf '<%s>\\n' \\\n        one \\\n        two\n    printf '%s\\n' pipeline \\\n        | string upper\nend\njoined\nprintf '%s\\n' \\\n# continued comment\nafter-comment\n";
    for ending in ["\n", "\r\n"] {
        fixture.check(
            &source.replace('\n', ending),
            b"<one>\n<two>\nPIPELINE\nafter-comment\n",
        );
    }
}

#[test]
fn continuations_join_keywords_words_and_substitution_tokens() {
    let fixture = Fixture::new();
    let source = "i\\\nf true\n    printf '<%s>\\n' he\\\nllo pre\\\n#post\ne\\\nnd\nprintf '<%s>\\n' (printf '%s' sub\\\n#token)\n";
    for ending in ["\n", "\r\n"] {
        fixture.check(
            &source.replace('\n', ending),
            b"<hello>\n<pre#post>\n<sub#token>\n",
        );
    }
}

#[test]
fn double_quoted_continuations_and_nested_substitutions_remove_the_line_ending() {
    let fixture = Fixture::new();
    let source = "printf '<%s>\\n' \"pre\\\npost\" \"$(printf '%s' inner\\\nvalue)\"\n";
    for ending in ["\n", "\r\n"] {
        fixture.check(&source.replace('\n', ending), b"<prepost>\n<innervalue>\n");
    }
}

#[test]
fn literal_carriage_returns_and_single_quoted_backslashes_are_preserved() {
    let fixture = Fixture::new();
    for ending in ["\n", "\r\n"] {
        // Join only the command boundaries: literal line endings inside quotes
        // are data and must survive unchanged for either script line ending.
        let source = [
            "printf '<%s>\\n' 'a\rb' \"a\rb\" a\\\rb",
            "printf '<%s>\\n' 'a\\\r\nb' \"a\r\nb\" \"a\\\\\r\nb\"",
            "",
        ]
        .join(ending);
        fixture.check(
            &source,
            b"<a\rb>\n<a\rb>\n<a\rb>\n<a\\\r\nb>\n<a\r\nb>\n<a\\\r\nb>\n",
        );
    }
}
