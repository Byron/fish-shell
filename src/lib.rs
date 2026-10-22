#![cfg_attr(
    all(nightly, feature = "benchmark"),
    feature(test),
    allow(unused_features)
)]

pub const BUILD_VERSION: &str = env!("FISH_BUILD_VERSION");

#[cfg(feature = "git")]
#[cfg_attr(not(windows), allow(dead_code))] // The native reference cache shares POSIX routing.
mod git;

pub(crate) mod argparse;
pub(crate) mod commandline_extent;
pub(crate) mod function_options;
pub mod history_core;
pub(crate) mod path_lexical;
pub(crate) mod random_core;
pub(crate) mod read_fields;
pub(crate) mod string_fields;
pub(crate) mod string_regex;
pub(crate) mod test_expression;
pub(crate) mod universal_variables;
pub(crate) mod variable_indices;

#[macro_use]
#[cfg(not(windows))]
pub mod common;

#[macro_use]
#[cfg(not(windows))]
pub mod wutil;

#[cfg(not(windows))]
pub mod abbrs;
#[cfg(not(windows))]
pub mod ast;
pub mod autoload;
#[cfg(not(windows))]
pub mod bash_script;
#[cfg(not(windows))]
pub mod bash_session;
#[cfg(not(windows))]
pub mod builtins;
#[cfg(not(windows))]
pub mod complete;
#[cfg(not(windows))]
pub mod editable_line;
#[cfg(not(windows))]
pub mod env;
#[cfg(not(windows))]
pub mod env_dispatch;
#[cfg(not(windows))]
pub mod env_universal_common;
#[cfg(not(windows))]
pub mod event;
#[cfg(not(windows))]
pub mod exec;
#[cfg(not(windows))]
pub mod expand;
#[cfg(not(windows))]
pub mod fd_monitor;
#[cfg(not(windows))]
pub mod fd_readable_set;
#[cfg(not(windows))]
pub mod fds;
#[cfg(not(windows))]
pub mod flog;
#[cfg(not(windows))]
pub mod fork_exec;
#[cfg(not(windows))]
pub mod fs;
#[cfg(not(windows))]
pub mod function;
#[cfg(not(windows))]
pub mod global_safety;
#[cfg(not(windows))]
pub mod highlight;
#[cfg(not(windows))]
pub mod history;
#[cfg(not(windows))]
pub mod input;
#[cfg(not(windows))]
pub mod io;
#[cfg(not(windows))]
pub mod job_group;
#[cfg(not(windows))]
pub mod key;
#[cfg(not(windows))]
pub mod kill;
#[cfg(not(windows))]
pub mod locale;
#[cfg(not(windows))]
pub mod localization;
#[cfg(not(windows))]
pub mod nix;
#[cfg(not(windows))]
pub mod null_terminated_array;
#[cfg(not(windows))]
pub mod operation_context;
#[cfg(not(windows))]
pub mod pager;
#[cfg(not(windows))]
pub mod panic;
#[cfg(not(windows))]
pub mod parse_constants;
#[cfg(not(windows))]
pub mod parse_execution;
#[cfg(not(windows))]
pub mod parse_tree;
#[cfg(not(windows))]
pub mod parse_util;
#[cfg(not(windows))]
pub mod parser;
#[cfg(not(windows))]
pub mod parser_keywords;
#[cfg(not(windows))]
pub mod path;
#[cfg(not(windows))]
pub mod portable_atomic;
#[cfg(not(windows))]
pub mod prelude;
#[cfg(not(windows))]
pub mod print_help;
#[cfg(not(windows))]
pub mod proc;
#[cfg(not(windows))]
pub mod re;
#[cfg(not(windows))]
pub mod reader;
#[cfg(not(windows))]
pub mod redirection;
#[cfg(not(windows))]
pub mod screen;
#[cfg(not(windows))]
pub mod signal;
#[cfg(not(windows))]
pub mod stdx;
#[cfg(not(windows))]
pub mod terminal;
#[cfg(not(windows))]
pub mod termsize;
#[cfg(not(windows))]
pub mod text_face;
#[cfg(not(windows))]
pub mod threads;
#[cfg(not(windows))]
pub mod timer;
#[cfg(not(windows))]
pub mod tinyexpr;
#[cfg(not(windows))]
pub mod tokenizer;
#[cfg(not(windows))]
pub mod topic_monitor;
#[cfg(not(windows))]
pub mod trace;
#[cfg(not(windows))]
pub mod tty_handoff;
#[cfg(not(windows))]
pub mod universal_notifier;
#[cfg(not(windows))]
pub mod wait_handle;
#[cfg(not(windows))]
pub mod wildcard;

#[cfg(feature = "gettext-extract")]
#[cfg(not(windows))]
pub extern crate fish_gettext_extraction;

#[cfg(not(windows))]
pub mod bash_input;
#[cfg(test)]
#[cfg(not(windows))]
mod bash_input_session;
#[cfg(not(windows))]
pub mod bash_syntax;
pub mod bash_to_fish;
#[cfg(test)]
#[cfg(not(windows))]
mod tests;

pub mod native;

#[cfg(windows)]
pub use native::syntax::{
    ast, common, flog, localization, parse_constants, parse_tree, parser_keywords, prelude,
    redirection, tokenizer, wutil,
};
