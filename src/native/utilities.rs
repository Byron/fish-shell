//! Packaged utilities are external-command fallbacks, not Fish builtins.
//!
//! Run each utility in a child of this executable so its standard streams,
//! localization, signal handlers and exit state cannot alter the shell.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
use std::process::Command;

#[cfg(feature = "native-utilities")]
#[path = "utility_awk.rs"]
mod awk;

const MARKER: &str = "--native-utility-v1";

pub(super) fn names() -> &'static [&'static str] {
    #[cfg(feature = "native-utilities")]
    {
        &[
            "awk", "basename", "cat", "comm", "cp", "cut", "date", "dirname", "du", "env", "expr",
            "find", "grep", "head", "ln", "ls", "mkdir", "mktemp", "mv", "readlink", "rm", "rmdir",
            "sed", "seq", "sleep", "sort", "tac", "tail", "tee", "touch", "tr", "uname", "uniq",
            "wc", "xargs",
        ]
    }
    #[cfg(not(feature = "native-utilities"))]
    {
        &[]
    }
}

pub(super) fn available(name: &str) -> bool {
    canonical_name(name).is_some()
}

fn canonical_name(name: &str) -> Option<&'static str> {
    names()
        .iter()
        .copied()
        .find(|entry| entry.eq_ignore_ascii_case(name))
}

pub(super) fn command(name: &str, args: &[OsString]) -> io::Result<Command> {
    let argv = invocation(name, args)?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    Ok(command)
}

pub(super) fn invocation(name: &str, args: &[OsString]) -> io::Result<Vec<OsString>> {
    let name = canonical_name(name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no packaged utility named {name}"),
        )
    })?;
    // uucore derives the utility name from the real process argv[1], even
    // when uumain receives a custom iterator. Keep the private marker second.
    let mut argv = vec![
        std::env::current_exe()?.into_os_string(),
        name.into(),
        MARKER.into(),
    ];
    argv.extend_from_slice(args);
    Ok(argv)
}

pub(super) fn run_if_requested() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    let name = args.next()?;
    if args.next().as_deref() != Some(OsStr::new(MARKER)) {
        return None;
    }
    let requested_name = name.to_str().unwrap_or_default();
    match canonical_name(requested_name) {
        #[cfg(feature = "native-utilities")]
        Some(name) => {
            // Fish has already expanded unquoted wildcards. uucore::args_os uses
            // wild on Windows and would expand quoted wildcard arguments again.
            let args = std::iter::once(OsString::from(name)).chain(args).collect();
            let status = run(name, args);
            let _ = io::stdout().flush();
            let _ = io::stderr().flush();
            Some(status)
        }
        _ => {
            let _ = writeln!(
                io::stderr(),
                "fish: no packaged utility named {requested_name}"
            );
            Some(127)
        }
    }
}

#[cfg(feature = "native-utilities")]
fn run(name: &str, args: Vec<OsString>) -> i32 {
    if name == "awk" {
        return awk::run(args);
    }
    if matches!(name, "find" | "xargs") {
        // findutils currently uses an older uucore than the other utilities.
        uucore_find::set_utility_is_second_arg();
        uucore_find::panic::mute_sigpipe_panic();
        let Ok(args) = args
            .into_iter()
            .map(OsString::into_string)
            .collect::<Result<Vec<_>, _>>()
        else {
            let _ = writeln!(io::stderr(), "{name}: arguments must be valid UTF-8");
            return 2;
        };
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        return if name == "find" {
            findutils::find::find_main(&args, &findutils::find::StandardDependencies::new())
        } else {
            findutils::xargs::xargs_main(&args)
        };
    }
    uucore::set_utility_is_second_arg();
    uucore::panic::preserve_inherited_sigpipe();
    uucore::panic::mute_sigpipe_panic();
    if let Err(error) = uucore::locale::setup_localization(name) {
        let _ = writeln!(
            io::stderr(),
            "{name}: cannot initialize localization: {error}"
        );
        return 99;
    }
    let args = args.into_iter();
    match name {
        "basename" => uu_basename::uumain(args),
        "cat" => uu_cat::uumain(args),
        "comm" => uu_comm::uumain(args),
        "cp" => uu_cp::uumain(args),
        "cut" => uu_cut::uumain(args),
        "date" => uu_date::uumain(args),
        "dirname" => uu_dirname::uumain(args),
        "du" => uu_du::uumain(args),
        "env" => uu_env::uumain(args),
        "expr" => uu_expr::uumain(args),
        "grep" => uu_grep::uumain(args),
        "head" => uu_head::uumain(args),
        "ln" => uu_ln::uumain(args),
        "ls" => uu_ls::uumain(args),
        "mkdir" => uu_mkdir::uumain(args),
        "mktemp" => uu_mktemp::uumain(args),
        "mv" => uu_mv::uumain(args),
        "readlink" => uu_readlink::uumain(args),
        "rm" => uu_rm::uumain(args),
        "rmdir" => uu_rmdir::uumain(args),
        "sed" => sed::sed::uumain(args),
        "seq" => uu_seq::uumain(args),
        "sleep" => uu_sleep::uumain(args),
        "sort" => uu_sort::uumain(args),
        "tac" => uu_tac::uumain(args),
        "tail" => uu_tail::uumain(args),
        "tee" => uu_tee::uumain(args),
        "touch" => uu_touch::uumain(args),
        "tr" => uu_tr::uumain(args),
        "uname" => uu_uname::uumain(args),
        "uniq" => uu_uniq::uumain(args),
        "wc" => uu_wc::uumain(args),
        _ => 127,
    }
}
