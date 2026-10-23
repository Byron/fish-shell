//! File-backed process substitution with command lifetimes, including workers.
use super::*;
use crate::native::temporary::Directory;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Reservation {
    path: PathBuf,
    owner: u32,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Resources {
    /// A command owns its directory even if a substitution's worker is killed
    /// before its updated interpreter snapshot can be returned.
    jobs: Vec<Option<Reservation>>,
    callers: Vec<usize>,
    pub(super) worker_root: Option<PathBuf>,
}

impl Resources {
    pub(super) fn has_job(&self) -> bool {
        !self.jobs.is_empty()
    }

    pub(super) fn push_job(&mut self) {
        self.jobs.push(None);
    }

    pub(super) fn pop_job(&mut self) {
        if let Some(Some(directory)) = self.jobs.pop() {
            if directory.owner == std::process::id() {
                let _ = std::fs::remove_dir_all(directory.path);
            }
        }
    }

    pub(super) fn push_substitution(&mut self) -> Result<()> {
        let index = self
            .jobs
            .len()
            .checked_sub(1)
            .ok_or_else(|| Error::new("command substitution has no caller"))?;
        self.callers.push(index);
        Ok(())
    }

    fn caller_directory(&mut self) -> Result<PathBuf> {
        let index = self
            .callers
            .last()
            .copied()
            .ok_or_else(|| Error::new("psub: not inside of command substitution"))?;
        if self.jobs[index].is_none() {
            let temporary = std::env::temp_dir();
            let parent = self.worker_root.as_ref().unwrap_or(&temporary);
            self.jobs[index] = Some(Reservation {
                path: Directory::create(parent)?.persist(),
                owner: std::process::id(),
            });
        }
        Ok(self.jobs[index].as_ref().unwrap().path.clone())
    }

    pub(super) fn prepare_worker(&mut self) -> Result<()> {
        // Reserve before cloning the session, so the parent can clean files
        // even if its worker dies before returning a snapshot. Pure in-process
        // substitutions need no filesystem work unless they actually use psub.
        if !self.callers.is_empty() {
            self.caller_directory()?;
        }
        Ok(())
    }

    pub(super) fn pop_substitution(&mut self) {
        self.callers.pop();
    }

    pub(super) fn retain_stopped(&mut self, pid: Option<u32>, host: &mut impl Host) -> Result<()> {
        let owned = |directory: &&Reservation| directory.owner == std::process::id();
        let paths = self
            .jobs
            .iter()
            .flatten()
            .filter(owned)
            // Most substitutions never call psub. An empty reservation needs
            // no job handle and can be removed by its ordinary scope cleanup.
            .filter(|directory| {
                std::fs::read_dir(&directory.path)
                    .map_or(true, |mut entries| entries.next().is_some())
            })
            .map(|directory| directory.path.clone())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return Ok(());
        }
        let pid = pid.ok_or_else(|| Error::new("stopped job has no process identifier"))?;
        host.retain_job_directories(pid, &paths)?;
        for directory in &mut self.jobs {
            if directory
                .as_ref()
                .is_some_and(|directory| paths.contains(&directory.path))
            {
                *directory = None;
            }
        }
        Ok(())
    }
}

impl Session {
    pub(super) fn psub_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let [suffix, testing] = args else {
            return Err(Error::new(
                "__fish_native_psub: expected suffix and testing mode",
            ));
        };
        if !matches!(testing.as_str(), "0" | "1") {
            return Err(Error::new("__fish_native_psub: invalid testing mode"));
        }
        if suffix.contains(['/', '\\', '\0', ':']) {
            return Err(Error::new(
                "psub: suffix must not contain a path separator or colon",
            ));
        }
        if self.psub_resources.callers.is_empty() {
            return Err(Error::new("psub: not inside of command substitution"));
        }
        // Testing mode intentionally leaves a file for the caller to remove,
        // matching the bundled function's private --testing contract.
        let testing_directory = (testing == "1")
            .then(|| Directory::create(&std::env::temp_dir()))
            .transpose()?;
        let directory = match &testing_directory {
            Some(directory) => directory.path().to_owned(),
            None => self.psub_resources.caller_directory()?,
        };
        let path = directory.join(format!(
            "psub-{}-{}{suffix}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        if !host.copy_input_to(&output.redirects, &mut file)? {
            self.status = 130;
            return Ok(());
        }
        drop(file);
        self.write(
            format!("{}\n", path.to_string_lossy()).as_bytes(),
            host,
            output,
        )?;
        if let Some(directory) = testing_directory {
            directory.persist();
        }
        self.status = 0;
        Ok(())
    }
}
