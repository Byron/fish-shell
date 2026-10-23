//! File-backed process substitution with command lifetimes, including workers.
use super::*;
use crate::native::temporary::Directory;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Reservation {
    path: PathBuf,
    owner: u32,
    // A worker may write into an inherited reservation, but cleanup ownership
    // never crosses process boundaries through interpreter snapshots.
    #[serde(skip)]
    lease: Option<Arc<Directory>>,
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
        self.jobs.pop();
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
            let directory = Arc::new(Directory::create(parent)?);
            self.jobs[index] = Some(Reservation {
                path: directory.path().to_owned(),
                owner: std::process::id(),
                lease: Some(directory),
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
        let resources = self
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
            .filter_map(|directory| directory.lease.clone())
            .map(|lease| lease as rash::host::JobResource)
            .collect::<Vec<_>>();
        if resources.is_empty() {
            return Ok(());
        }
        let pid = pid.ok_or_else(|| Error::new("stopped job has no process identifier"))?;
        // Both the enclosing consumer and the stopped job may continue using
        // these files. Cleanup waits for both lifetimes, in either order.
        host.retain_job_resources(pid, &resources)
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

#[cfg(test)]
#[path = "fish_psub_tests.rs"]
mod tests;
