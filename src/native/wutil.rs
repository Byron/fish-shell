//! Platform-independent wide-string utilities used by the native backend.
#[path = "../wutil/errors.rs"]
mod errors;
#[path = "../wutil/hex_float.rs"]
mod hex_float;
#[path = "../wutil/printf.rs"]
mod printf;
#[path = "../wutil/wcstod.rs"]
pub mod wcstod;
#[path = "../wutil/wcstoi.rs"]
pub mod wcstoi;

pub use errors::Error;
pub use printf::{eprintf, fprintf, printf, sprintf};
pub use wcstoi::{fish_wcstol, fish_wcstoul, wcstoi_opts};
