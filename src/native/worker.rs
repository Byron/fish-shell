//! Private interpreter snapshots for native pipeline processes.
use bincode::Options as _;
use rash::{Error, Result};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8] = b"fish-native-worker\0\x01";
const LIMIT: u64 = 64 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct PayloadFile(PathBuf);

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
                Ok(file) => return Ok((Self(path), file)),
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

pub fn prepare(
    language: &str,
    state: Vec<u8>,
    env: BTreeMap<Vec<u8>, Vec<u8>>,
    display: Vec<u8>,
) -> Result<(rash::host::Command, PayloadFile)> {
    if !matches!(language, "fish" | "rash") || state.len() as u64 > LIMIT {
        return Err(Error::new("invalid interpreter snapshot"));
    }
    let (payload, mut file) = PayloadFile::create()?;
    file.write_all(MAGIC)?;
    file.write_all(&state)?;
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
            builtin: false,
        },
        payload,
    ))
}

pub fn run(language: &str, path: &Path) -> Result<i32> {
    let valid_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("fish-native-worker-") && n.ends_with(".bin"));
    if !valid_name {
        return Err(Error::new("invalid interpreter snapshot filename"));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(LIMIT + MAGIC.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT + MAGIC.len() as u64 || !bytes.starts_with(MAGIC) {
        return Err(Error::new("invalid interpreter snapshot header or size"));
    }
    let mut host = super::host::NativeHost::new_worker()?;
    let state = &bytes[MAGIC.len()..];
    match language {
        "rash" => {
            // Validate before unlinking. The parent guard tolerates the file's removal.
            let snapshot = rash::Session::decode_worker(state)?;
            std::fs::remove_file(path)?;
            rash::Session::run_worker(snapshot, &mut host)
        }
        #[cfg(windows)]
        "fish" => {
            let snapshot = decode(state)?;
            std::fs::remove_file(path)?;
            super::fish::Session::run_worker(snapshot, &mut host)
        }
        _ => Err(Error::new("unknown interpreter snapshot language")),
    }
}
