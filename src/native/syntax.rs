//! The existing Fish grammar, with platform-independent parser services.
//!
//! Execution remains a separate backend. Keeping these modules shared ensures that
//! native Windows Fish parses Fish syntax rather than a second shell language.

#[path = "../ast.rs"]
pub mod ast;
#[path = "../localization/gettext.rs"]
pub mod localization;
#[path = "../parse_constants.rs"]
pub mod parse_constants;
#[path = "../parse_token.rs"]
pub mod parse_tree;
#[path = "../parser_keywords.rs"]
pub mod parser_keywords;
#[path = "../redirection_mode.rs"]
pub mod redirection;
#[path = "../tokenizer.rs"]
pub mod tokenizer;
#[path = "../wutil/printf.rs"]
pub mod wutil;

pub mod prelude {
    pub use super::localization::{
        LocalizableString, localizable_consts, localizable_string, wgettext, wgettext_fmt,
    };
    pub use super::wutil::sprintf;
    pub use fish_widestring::prelude::*;
}

pub mod common {
    pub fn valid_var_name_char(c: char) -> bool {
        // Private-use characters are not alphanumeric according to Rust's Unicode
        // tables; the explicit exclusion also protects Fish's byte-encoding range.
        c == '_' || (!fish_widestring::fish_reserved_codepoint(c) && c.is_alphanumeric())
    }
}

/// Parser diagnostics use stderr without POSIX descriptors or signal machinery.
pub mod flog {
    pub fn enabled(category: &str) -> bool {
        category == "debug"
            || std::env::var("FISH_DEBUG")
                .is_ok_and(|value| value.split(',').any(|item| item == category || item == "*"))
    }

    #[macro_export]
    macro_rules! native_parser_flog {
        ($category:ident, $($elem:expr),+ $(,)*) => {
            if $crate::flog::enabled(stringify!($category)) {
                use std::io::Write as _;
                let mut output = std::io::stderr().lock();
                let _ = write!(output, "{}:", stringify!($category));
                $(let _ = write!(output, " {}", $elem);)+
                let _ = writeln!(output);
            }
        };
    }

    #[macro_export]
    macro_rules! native_parser_flogf {
        ($category:ident, $fmt:expr, $($elem:expr),+ $(,)*) => {
            $crate::native_parser_flog!($category, $crate::wutil::sprintf!($fmt, $($elem),*))
        };
    }
    pub use {native_parser_flog as flog, native_parser_flogf as flogf};
}
