//! Conversion between shell bytes and native operating-system strings.
//!
//! Unix preserves arbitrary bytes. Windows uses UTF-8 at the shell boundary;
//! values that cannot be represented are rejected instead of silently changing
//! an argument or selecting a different file.

use std::ffi::{OsStr, OsString};

use crate::Result;

pub fn from_bytes(bytes: &[u8]) -> Result<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        Ok(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(windows)]
    {
        std::str::from_utf8(bytes)
            .map(OsString::from)
            .map_err(|_| invalid_encoding("native Windows paths and arguments require UTF-8"))
    }
}

pub fn to_bytes(value: &OsStr) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Ok(value.as_bytes().to_vec())
    }
    #[cfg(windows)]
    {
        value
            .to_str()
            .map(|value| value.as_bytes().to_vec())
            .ok_or_else(|| {
                invalid_encoding("native Windows string contains unpaired UTF-16 surrogates")
            })
    }
}

#[cfg(windows)]
fn invalid_encoding(message: &str) -> crate::Error {
    crate::Error {
        message: message.into(),
        status: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_round_trip() {
        let bytes = "directory with spaces/🐟-é".as_bytes();
        assert_eq!(to_bytes(&from_bytes(bytes).unwrap()).unwrap(), bytes);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_round_trip() {
        assert_eq!(
            to_bytes(&from_bytes(b"raw-\xff").unwrap()).unwrap(),
            b"raw-\xff"
        );
    }

    #[test]
    #[cfg(windows)]
    fn invalid_encodings_are_rejected() {
        use std::os::windows::ffi::OsStringExt as _;
        assert!(from_bytes(b"raw-\xff").is_err());
        assert!(to_bytes(&OsString::from_wide(&[0xd800])).is_err());
    }
}
