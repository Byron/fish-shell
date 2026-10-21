//! Explicit Bash script evaluation in the in-process Rash session.

use crate::env::Statuses;
use crate::parser::Parser;
use crate::prelude::*;
use fish_widestring::{bytes2wcstring, wcs2bytes};
use std::io;
use std::os::unix::ffi::OsStringExt as _;

pub fn run(
    parser: &mut Parser,
    script_args: &[WString],
    startup: bool,
    login: bool,
    syntax_only: bool,
) -> io::Result<()> {
    assert!(!script_args.is_empty());
    let source = std::fs::read(std::ffi::OsString::from_vec(wcs2bytes(&script_args[0])))?;
    let mut session = crate::bash_session::Session::start(parser, false, login, false)?;
    session.set_script_args(script_args);
    if syntax_only {
        let (status, error) = session.check_syntax(&bytes2wcstring(&source))?;
        if status != 0 {
            eprintf!("rash: %s\n", error);
        }
        parser.set_last_statuses(Statuses::just(if status == 0 { 0 } else { 2 }));
    } else {
        if startup {
            session.startup(parser)?;
        }
        if session.exited.is_none() {
            session.eval_bytes(&source, parser)?;
        }
        session.finish(parser)?;
    }
    Ok(())
}
