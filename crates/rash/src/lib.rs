//! A source-guided Rust port of GNU Bash 3.2.
//!
//! Fish embeds Session directly and supplies Host process, I/O and signal services.
//! Parsing and expansion stay in Rust; no native Bash runtime is used. This is
//! an experimental, incomplete port. See the README for tested behavior and limits.

#![forbid(unsafe_code)]

pub mod ansi_c;
pub mod quote;
pub mod status;

pub mod arithmetic;
pub mod builtins_printf;
pub mod expand;
pub mod host;
pub mod os;
pub mod session;
pub mod syntax;

pub use session::{Session, Value, Variable};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub message: String,
    pub status: i32,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: 2,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self {
            message: error.to_string(),
            status: 1,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
