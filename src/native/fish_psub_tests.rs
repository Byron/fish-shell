use super::*;
use rash::host::{JobResource, ReadResult, RunResult};

#[derive(Default)]
struct StoppedHost(Vec<JobResource>);

impl Host for StoppedHost {
    fn retain_job_resources(&mut self, pid: u32, resources: &[JobResource]) -> Result<()> {
        assert_eq!(pid, 42);
        self.0.extend(resources.iter().cloned());
        Ok(())
    }
    fn run_pipeline(&mut self, _: &[Command], _: bool, _: bool) -> Result<RunResult> {
        unreachable!()
    }
    fn change_dir(&mut self, _: &[u8]) -> Result<Vec<u8>> {
        unreachable!()
    }
    fn write(&mut self, _: i32, _: &[u8], _: &[Redirection], _: bool) -> Result<Vec<u8>> {
        unreachable!()
    }
    fn read_line(&mut self, _: &[Redirection], _: bool) -> Result<ReadResult> {
        unreachable!()
    }
    fn cancelled(&self) -> bool {
        false
    }
    fn jobs(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

fn reserved() -> (Resources, PathBuf) {
    let mut resources = Resources::default();
    resources.push_job();
    resources.push_substitution().unwrap();
    let path = resources.caller_directory().unwrap().join("consumer-file");
    std::fs::write(&path, b"still needed").unwrap();
    resources.pop_substitution();
    (resources, path)
}

#[test]
fn enclosing_consumer_and_stopped_job_release_the_file_in_either_order() {
    for consumer_finishes_first in [false, true] {
        let (mut resources, path) = reserved();
        let mut host = StoppedHost::default();
        resources.retain_stopped(Some(42), &mut host).unwrap();
        assert_eq!(host.0.len(), 1);
        if consumer_finishes_first {
            resources.pop_job();
            assert!(path.exists());
            host.0.clear();
        } else {
            host.0.clear();
            assert!(path.exists());
            resources.pop_job();
        }
        assert!(!path.exists());
    }
}

#[test]
fn snapshots_share_paths_without_acquiring_the_owners_cleanup_lease() {
    let (mut resources, path) = reserved();
    let encoded = crate::native::worker::encode(&resources).unwrap();
    let mut worker: Resources = crate::native::worker::decode(&encoded).unwrap();
    assert_eq!(
        worker.jobs[0].as_ref().unwrap().path,
        path.parent().unwrap()
    );
    assert!(worker.jobs[0].as_ref().unwrap().lease.is_none());
    worker.pop_job();
    assert!(path.exists(), "a snapshot removed its parent's file");
    resources.pop_job();
    assert!(!path.exists());
}
