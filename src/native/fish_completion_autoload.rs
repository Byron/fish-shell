//! Native completion evaluation using Fish's shared autoload resolver.
use super::*;
use crate::autoload::{Autoload, AutoloadResult};
use crate::native::interactive_completion::CompletionEngine;
use fish_widestring::L;
use std::collections::BTreeSet;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Loader {
    #[serde(with = "serialization")]
    resolver: Autoload,
    paths: Vec<String>,
    tombstones: BTreeSet<String>,
}

impl Default for Loader {
    fn default() -> Self {
        Self {
            resolver: Autoload::new(L!("fish_complete_path")),
            paths: Vec::new(),
            tombstones: BTreeSet::new(),
        }
    }
}

mod serialization {
    use super::*;
    use serde::{Deserialize, Serialize};

    pub fn serialize<S: serde::Serializer>(
        loader: &Autoload,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        loader.snapshot().serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Autoload, D::Error> {
        let state = crate::autoload::AutoloadState::deserialize(deserializer)?;
        let mut loader = Autoload::new(L!("fish_complete_path"));
        loader.restore(state);
        Ok(loader)
    }
}

impl Session {
    pub(super) fn refresh_completion_paths(&mut self) {
        let paths = self.values("fish_complete_path");
        if self.completion_autoload.paths == paths {
            return;
        }
        let loaded: BTreeSet<_> = self
            .completion_autoload
            .resolver
            .get_autoloaded_commands()
            .into_iter()
            .map(|name| name.to_string())
            .collect();
        // Match POSIX's command-level invalidation. Registrations for other
        // commands, including explicit path registrations, remain untouched.
        self.completions
            .retain(|entry| entry.is_path || !loaded.contains(&entry.command));
        self.completion_autoload.resolver.clear();
        self.completion_autoload.paths = paths;
    }

    pub(super) fn prevent_completion_autoload(&mut self, command: &str) {
        self.completion_autoload
            .tombstones
            .insert(command.to_owned());
    }

    /// Load before freezing the query's registrations. A function or completion
    /// file can introduce a wrap target whose completions are needed immediately.
    pub(super) fn autoload_completions(
        &mut self,
        line: &str,
        pos: usize,
        cwd: &std::path::Path,
        host: &mut impl Host,
        engine: &mut CompletionEngine,
    ) -> Result<()> {
        self.refresh_function_paths();
        self.refresh_completion_paths();
        let mut attempted = BTreeSet::new();
        loop {
            let (commands, path, variables) = self.completion_context();
            engine.set_context(commands, path, variables);
            engine.set_input_language(self.input_language);
            engine.set_registrations(self.completions.clone());
            engine.set_wrappers(self.completion_wrappers());
            let pending = engine.autoload_commands(line, pos, cwd);
            let mut loaded = false;
            for (name, context) in pending {
                if host.cancelled() || self.flow == Flow::Exit || attempted.len() >= 64 {
                    return Ok(());
                }
                if !attempted.insert(name.clone()) {
                    continue;
                }
                loaded = true;
                self.completion_line = Some(context);
                self.load_completion(&name, host)?;
            }
            if !loaded {
                return Ok(());
            }
        }
    }

    fn load_completion(&mut self, name: &str, host: &mut impl Host) -> Result<()> {
        if self.completion_autoload.tombstones.contains(name) {
            return Ok(());
        }
        let mut output = Output::default();
        self.ensure_function(name, host, &mut output)?;
        if self.flow == Flow::Exit || host.cancelled() {
            return Ok(());
        }
        self.refresh_completion_paths();
        let name = WString::from(name);
        let paths: Vec<_> = self
            .completion_autoload
            .paths
            .iter()
            // The shared resolver puts generated completions after embedded
            // ones. Its directory classification uses Fish's slash spelling.
            .map(|path| WString::from(path.replace('\\', "/")))
            .collect();
        let found = self
            .completion_autoload
            .resolver
            .resolve_command_from_paths(&name, &paths);
        if let AutoloadResult::Path(path) = found {
            let status = self.status;
            let pipeline = self.pipeline.clone();
            let flow = std::mem::replace(&mut self.flow, Flow::Normal);
            let result = (|| {
                let (origin, source) = autoload::read_source(path)?;
                self.eval_sourced(&source, origin, &[], true, host, &mut output)
            })();
            self.completion_autoload
                .resolver
                .mark_autoload_finished(&name);
            self.status = status;
            self.pipeline = pipeline;
            if self.flow != Flow::Exit {
                self.flow = flow;
            }
            result?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rash::host::{ReadResult, RunResult};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Directory(PathBuf);

    impl Directory {
        fn new(source: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "fish-completion-loader-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            let directory = Self(path);
            directory.write(source);
            directory
        }

        fn write(&self, source: &str) {
            std::fs::write(self.0.join("native_probe.fish"), source).unwrap();
        }

        fn session(&self, host: &mut TestHost) -> Session {
            let mut session = Session::new().unwrap();
            session.eval("function native_probe; end", host).unwrap();
            session.variables.insert(
                "fish_complete_path".into(),
                Variable {
                    values: vec![self.0.to_string_lossy().into_owned()],
                    exported: false,
                    pathvar: false,
                },
            );
            session
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.join("native_probe.fish"));
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    struct TestHost;

    impl Host for TestHost {
        fn run_pipeline(&mut self, _: &[Command], _: bool, _: bool) -> Result<RunResult> {
            Err(Error::new("unexpected external process"))
        }
        fn change_dir(&mut self, _: &[u8]) -> Result<Vec<u8>> {
            unreachable!()
        }
        fn write(
            &mut self,
            _: i32,
            bytes: &[u8],
            _: &[Redirection],
            capture: bool,
        ) -> Result<Vec<u8>> {
            Ok(if capture { bytes.to_vec() } else { Vec::new() })
        }
        fn read_line(&mut self, _: &[Redirection], _: bool) -> Result<ReadResult> {
            Ok(ReadResult::Line(Vec::new()))
        }
        fn cancelled(&self) -> bool {
            false
        }
        fn jobs(&self) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }

    #[test]
    fn failed_source_restores_query_state_and_finishes_autoload() {
        let directory = Directory::new("set -g loaded yes; fish_input_mode bash");
        let mut host = TestHost;
        let mut session = directory.session(&mut host);
        session.status = 7;
        session.pipeline = vec![9, 7];
        let context = super::super::super::interactive_completion::CompletionContext {
            line: "saved".into(),
            cursor: 5,
        };
        session.completion_line = Some(context.clone());
        let mut engine = CompletionEngine::new("fish");
        assert!(
            session
                .completion_query("native_probe a", 14, &directory.0, &mut host, &mut engine)
                .is_err()
        );
        assert_eq!(session.status, 7);
        assert_eq!(session.pipeline, [9, 7]);
        assert_eq!(session.completion_line, Some(context));
        assert_eq!(session.completion_depth, 0);
        assert!(!session.input_mode_blocked);
        assert!(session.flow == Flow::Normal);
        assert!(
            !session
                .completion_autoload
                .resolver
                .autoload_in_progress(L!("native_probe"))
        );
        assert_eq!(session.values("loaded"), ["yes"]);
        assert!(
            session
                .completion_query("native_probe a", 14, &directory.0, &mut host, &mut engine)
                .is_ok()
        );
    }

    #[test]
    fn autoload_exit_survives_query_cleanup() {
        let directory = Directory::new("exit 23");
        let mut host = TestHost;
        let mut session = directory.session(&mut host);
        let mut engine = CompletionEngine::new("fish");
        let (_, candidates) = session
            .completion_query("native_probe a", 14, &directory.0, &mut host, &mut engine)
            .unwrap();
        assert!(candidates.is_empty());
        assert!(session.flow == Flow::Exit);
        assert_eq!(session.completion_depth, 0);
        assert!(!session.input_mode_blocked);
        assert!(
            !session
                .completion_autoload
                .resolver
                .autoload_in_progress(L!("native_probe"))
        );
    }

    #[test]
    fn serialized_loader_rechecks_the_file_identity_that_was_evaluated() {
        let directory = Directory::new("complete native_probe -f -a alpha");
        let mut host = TestHost;
        let mut session = directory.session(&mut host);
        let mut engine = CompletionEngine::new("fish");
        let first = session
            .completion_query("native_probe a", 14, &directory.0, &mut host, &mut engine)
            .unwrap()
            .1;
        assert_eq!(first[0].replacement, "alpha ");
        let snapshot = crate::native::worker::encode(&session).unwrap();
        directory.write("complete native_probe -f -a beta_changed");
        let mut restored: Session = crate::native::worker::decode(&snapshot).unwrap();
        let second = restored
            .completion_query("native_probe b", 14, &directory.0, &mut host, &mut engine)
            .unwrap()
            .1;
        assert_eq!(second[0].replacement, "beta_changed ");
    }
}
