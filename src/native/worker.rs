//! Private interpreter snapshots for native pipeline processes.
use bincode::Options as _;
use rash::{Error, Result};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8] = b"fish-native-worker\0\x03";
const LIMIT: u64 = 64 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope {
    state: Vec<u8>,
    jobs: Vec<super::host::JobView>,
    scratch: PathBuf,
}

pub struct PayloadFile(PathBuf, Option<super::temporary::Directory>);

impl PayloadFile {
    pub fn create() -> Result<(Self, File)> {
        for _ in 0..64 {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "fish-native-worker-{}-{stamp}-{}.bin",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => return Ok((Self(path, None), file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::new("unable to create interpreter snapshot"))
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PayloadFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    bincode::DefaultOptions::new()
        .with_limit(LIMIT)
        .serialize(value)
        .map_err(|e| Error::new(format!("interpreter snapshot: {e}")))
}

pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    bincode::DefaultOptions::new()
        .with_limit(LIMIT)
        .deserialize(bytes)
        .map_err(|e| Error::new(format!("interpreter snapshot: {e}")))
}

pub(crate) fn prepare(
    language: &str,
    state: Vec<u8>,
    jobs: Vec<super::host::JobView>,
    env: BTreeMap<Vec<u8>, Vec<u8>>,
    display: Vec<u8>,
) -> Result<(rash::host::Command, PayloadFile)> {
    if !matches!(language, "fish" | "rash") || state.len() as u64 > LIMIT {
        return Err(Error::new("invalid interpreter snapshot"));
    }
    let scratch = super::temporary::Directory::create(&std::env::temp_dir())?;
    let bytes = encode(&Envelope {
        state,
        jobs,
        scratch: scratch.path().to_owned(),
    })?;
    let (mut payload, mut file) = PayloadFile::create()?;
    payload.1 = Some(scratch);
    file.write_all(MAGIC)?;
    file.write_all(&bytes)?;
    file.flush()?;
    Ok((
        rash::host::Command {
            argv: vec![
                rash::os::to_bytes(std::env::current_exe()?.as_os_str())?,
                format!("--native-worker={language}").into_bytes(),
                rash::os::to_bytes(payload.path().as_os_str())?,
            ],
            env,
            display: Some(display),
            redirects: vec![],
            pipe_fd: 1,
            builtin: false,
        },
        payload,
    ))
}

fn read_envelope(path: &Path) -> Result<Envelope> {
    let valid_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("fish-native-worker-") && n.ends_with(".bin"));
    if !valid_name {
        return Err(Error::new("invalid interpreter snapshot filename"));
    }
    let file = File::open(path)?;
    if file.metadata()?.len() > LIMIT + MAGIC.len() as u64 {
        return Err(Error::new("invalid interpreter snapshot header or size"));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + MAGIC.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT + MAGIC.len() as u64 || !bytes.starts_with(MAGIC) {
        return Err(Error::new("invalid interpreter snapshot header or size"));
    }
    decode(&bytes[MAGIC.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_envelope_preserves_opaque_state_and_rejects_old_or_truncated_payloads() {
        let (_, payload) = prepare(
            "rash",
            vec![1, 2, 3],
            Vec::new(),
            BTreeMap::new(),
            Vec::new(),
        )
        .unwrap();
        let envelope = read_envelope(payload.path()).unwrap();
        assert_eq!(envelope.state, [1, 2, 3]);
        assert!(envelope.jobs.is_empty());
        std::fs::write(payload.path(), b"fish-native-worker\0\x01old-format").unwrap();
        assert!(read_envelope(payload.path()).is_err());
        std::fs::write(payload.path(), MAGIC).unwrap();
        assert!(read_envelope(payload.path()).is_err());
        assert!(
            payload.path().exists(),
            "invalid input must not be unlinked by validation"
        );
    }

    #[test]
    fn worker_envelope_size_limit_covers_the_complete_payload() {
        let (payload, mut file) = PayloadFile::create().unwrap();
        file.write_all(MAGIC).unwrap();
        file.set_len(LIMIT + MAGIC.len() as u64 + 1).unwrap();
        drop(file);
        assert!(read_envelope(payload.path()).is_err());
    }
}

pub fn run(language: &str, path: &Path) -> Result<i32> {
    let Envelope {
        state,
        jobs,
        scratch,
    } = read_envelope(path)?;
    // Declared before the host so owned children and their resource leases are
    // released first. The parent independently handles forced worker death.
    let _scratch = super::temporary::EmptyDirectory(scratch.clone());
    #[cfg(not(windows))]
    let _ = scratch;
    let mut host = super::host::NativeHost::new_worker(jobs)?;
    match language {
        "rash" => {
            // Validate before unlinking. The parent guard tolerates the file's removal.
            let snapshot = rash::Session::decode_worker(&state)?;
            std::fs::remove_file(path)?;
            rash::Session::run_worker(snapshot, &mut host)
        }
        #[cfg(windows)]
        "fish" => {
            let snapshot = decode(&state)?;
            std::fs::remove_file(path)?;
            super::fish::Session::run_worker(snapshot, scratch, &mut host)
        }
        _ => Err(Error::new("unknown interpreter snapshot language")),
    }
}
