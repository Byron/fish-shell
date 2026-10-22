use super::prelude::*;
use crate::test_expression::{self, Context, FileComparison, FilePermission, StatPredicate};
use crate::wutil::{FileId, file_id_for_path, lwstat, waccess, wstat};
use crate::{err_raw, err_str, should_flog};
use fish_feature_flags::{FeatureFlag, feature_test};
use nix::unistd::{AccessFlags, Gid, Uid};
use std::os::unix::prelude::*;

struct PosixContext<'a, 'b> {
    streams: &'a mut IoStreams<'b>,
}

/// Return true if \param rhs has higher mtime seconds than this file_id_t.
/// If identical, nanoseconds are compared.
fn older_than(lhs: Option<&FileId>, rhs: Option<&FileId>) -> bool {
    fn mod_time(file_id: Option<&FileId>) -> (i64, i64) {
        match file_id {
            Some(file_id) => (file_id.mod_seconds, file_id.mod_nanoseconds),
            None => (i64::MIN, i64::MIN),
        }
    }
    mod_time(lhs).cmp(&mod_time(rhs)).is_lt()
}

impl Context for PosixContext<'_, '_> {
    #[allow(clippy::unnecessary_cast)]
    fn file_stat(&mut self, predicate: StatPredicate, path: &wstr) -> Result<bool, WString> {
        let Ok(md) = wstat(path) else {
            return Ok(false);
        };
        Ok(match predicate {
            StatPredicate::b => md.file_type().is_block_device(),
            StatPredicate::c => md.file_type().is_char_device(),
            StatPredicate::d => md.file_type().is_dir(),
            StatPredicate::e => true,
            StatPredicate::f => md.file_type().is_file(),
            StatPredicate::G => md.gid() == Gid::effective().as_raw(),
            StatPredicate::g => md.permissions().mode() & libc::S_ISGID as u32 != 0,
            StatPredicate::k => md.permissions().mode() & libc::S_ISVTX as u32 != 0,
            StatPredicate::O => md.uid() == Uid::effective().as_raw(),
            StatPredicate::p => md.file_type().is_fifo(),
            StatPredicate::S => md.file_type().is_socket(),
            StatPredicate::s => md.len() > 0,
            StatPredicate::u => md.permissions().mode() & libc::S_ISUID as u32 != 0,
        })
    }

    fn file_permission(
        &mut self,
        permission: FilePermission,
        path: &wstr,
    ) -> Result<bool, WString> {
        let mode = match permission {
            FilePermission::r => AccessFlags::R_OK,
            FilePermission::w => AccessFlags::W_OK,
            FilePermission::x => AccessFlags::X_OK,
        };
        Ok(waccess(path, mode).is_ok())
    }

    fn is_symlink(&mut self, path: &wstr) -> Result<bool, WString> {
        Ok(lwstat(path).is_ok_and(|md| md.file_type().is_symlink()))
    }

    fn compare_files(
        &mut self,
        comparison: FileComparison,
        left: &wstr,
        right: &wstr,
    ) -> Result<bool, WString> {
        let left = file_id_for_path(left).ok();
        let right = file_id_for_path(right).ok();
        Ok(match comparison {
            FileComparison::Newer => older_than(right.as_ref(), left.as_ref()),
            FileComparison::Older => older_than(left.as_ref(), right.as_ref()),
            FileComparison::Same => left == right,
        })
    }

    fn is_terminal(&mut self, fd: i32) -> Result<bool, WString> {
        Ok(match fd {
            0 => match self.streams.stdin_fd() {
                -1 => false,
                fd => crate::nix::isatty(fd),
            },
            1 => !self.streams.out_is_redirected && crate::nix::isatty(libc::STDOUT_FILENO),
            2 => !self.streams.err_is_redirected && crate::nix::isatty(libc::STDERR_FILENO),
            fd => crate::nix::isatty(fd),
        })
    }
}

/// Evaluate a conditional expression given the arguments. For POSIX conformance this
/// supports a more limited range of functionality.
/// Return status is the final shell status, i.e. 0 for true, 1 for false and 2 for error.
pub fn test(parser: &mut Parser, streams: &mut IoStreams, argv: &mut [&wstr]) -> BuiltinResult {
    // The first argument should be the name of the command ('test').
    if argv.is_empty() {
        return Err(STATUS_INVALID_ARGS);
    }

    // Whether we are invoked with bracket '[' or not.
    let program_name = argv[0];
    let is_bracket = program_name == "[";

    let mut argc = argv.len() - 1;

    // If we're bracket, the last argument ought to be ]; we ignore it. Note that argc is the number
    // of arguments after the command name; thus argv[argc] is the last argument.
    if is_bracket {
        if argv[argc] == "]" {
            // Ignore the closing bracket from now on.
            argc -= 1;
        } else {
            err_str!("the last argument must be ']'")
                .cmd(program_name)
                .full_trailer(parser)
                .finish(streams);
            return Err(STATUS_INVALID_ARGS);
        }
    }

    // Collect the arguments into a list.
    let args: Vec<WString> = argv[1..=argc].iter().map(|&arg| arg.to_owned()).collect();
    let args: &[WString] = &args;

    if feature_test(FeatureFlag::TestRequireArg) {
        if argc == 0 {
            err_str!("Expected at least one argument")
                .cmd(program_name)
                .full_trailer(parser)
                .finish(streams);
            return Err(STATUS_INVALID_ARGS);
        } else if argc == 1 {
            if args[0] == "-n" {
                return Err(STATUS_CMD_ERROR);
            } else if args[0] == "-z" {
                return Ok(SUCCESS);
            }
        }
    } else if argc == 0 {
        if should_flog!(deprecated_test) {
            err_str!("called with no arguments. This will be an error in future.")
                .cmd(program_name)
                .stacktrace(parser)
                .finish(streams);
        }
        return Err(STATUS_INVALID_ARGS); // Per 1003.1, exit false.
    } else if argc == 1 {
        if should_flog!(deprecated_test) && args[0] != "-z" {
            err_str!("called with one argument. This will return false in future.")
                .cmd(program_name)
                .stacktrace(parser)
                .finish(streams);
        }
        // Per 1003.1, exit true if the arg is non-empty.
        return if args[0].is_empty() {
            Err(STATUS_CMD_ERROR)
        } else {
            Ok(SUCCESS)
        };
    }

    // Try parsing
    let expr = test_expression::TestParser::parse_args(args);
    let expr = match expr {
        Ok(expr) => expr,
        Err(err) => {
            err_raw!(err)
                .cmd(program_name)
                .stacktrace(parser)
                .finish(streams);
            return Err(STATUS_CMD_ERROR);
        }
    };

    let mut eval_errors = Vec::new();
    let result = expr.evaluate(&mut PosixContext { streams }, &mut eval_errors);
    if !eval_errors.is_empty() {
        for eval_error in eval_errors {
            streams.err.appendln(&eval_error);
        }
        // Add a backtrace but not the "see help" message
        // because this isn't about passing the wrong options.
        streams.err.append(&parser.current_line());
        return Err(STATUS_INVALID_ARGS);
    }

    if result {
        Ok(SUCCESS)
    } else {
        Err(STATUS_CMD_ERROR)
    }
}

#[cfg(test)]
mod tests {
    use super::test as builtin_test;
    use crate::{
        builtins::prelude::*,
        io::{IoChain, OutputStream},
        tests::prelude::*,
    };
    use fish_widestring::str2wcstring;

    fn run_one_test_test_mbracket(expected: i32, lst: &[&str], bracket: bool) -> bool {
        let parser = &mut TestParser::new();
        let mut argv = Vec::new();
        if bracket {
            argv.push(L!("[").to_owned());
        } else {
            argv.push(L!("test").to_owned());
        }
        for s in lst {
            argv.push(str2wcstring(s));
        }
        if bracket {
            argv.push(L!("]").to_owned());
        }

        // Convert to &[&wstr].
        let mut argv = argv.iter().map(|s| s.as_ref()).collect::<Vec<_>>();
        let mut out = OutputStream::Null;
        let mut err = OutputStream::Null;
        let io_chain = IoChain::new();
        let mut streams = IoStreams::new(&mut out, &mut err, &io_chain);

        let result = builtin_test(parser, &mut streams, &mut argv).builtin_status_code();

        if result != expected {
            eprintf!(
                "expected builtin_test() to return %s, got %s\n",
                expected.to_string(),
                result.to_string()
            );
        }
        result == expected
    }

    fn run_test_test(expected: i32, lst: &[&str]) -> bool {
        let nobracket = run_one_test_test_mbracket(expected, lst, false);
        let bracket = run_one_test_test_mbracket(expected, lst, true);
        assert_eq!(nobracket, bracket);
        nobracket
    }

    fn test_test_brackets() {
        // Ensure [ knows it needs a ].
        let parser = &mut TestParser::new();

        let mut out = OutputStream::Null;
        let mut err = OutputStream::Null;
        let io_chain = IoChain::new();
        let mut streams = IoStreams::new(&mut out, &mut err, &io_chain);

        let args1 = &mut [L!("["), L!("foo")];
        assert_eq!(
            builtin_test(parser, &mut streams, args1),
            Err(STATUS_INVALID_ARGS)
        );

        let args2 = &mut [L!("["), L!("foo"), L!("]")];
        assert_eq!(builtin_test(parser, &mut streams, args2), Ok(SUCCESS));

        let args3 = &mut [L!("["), L!("foo"), L!("]"), L!("bar")];
        assert_eq!(
            builtin_test(parser, &mut streams, args3),
            Err(STATUS_INVALID_ARGS)
        );
    }

    #[rustfmt::skip]
    fn test_test() {
        assert!(run_test_test(0, &["5", "-ne", "6"]));
        assert!(run_test_test(0, &["5", "-eq", "5"]));
        assert!(run_test_test(0, &["0", "-eq", "0"]));
        assert!(run_test_test(0, &["-1", "-eq", "-1"]));
        assert!(run_test_test(0, &["1", "-ne", "-1"]));
        assert!(run_test_test(1, &[" 2 ", "-ne", "2"]));
        assert!(run_test_test(0, &[" 2", "-eq", "2"]));
        assert!(run_test_test(0, &["2 ", "-eq", "2"]));
        assert!(run_test_test(0, &[" 2 ", "-eq", "2"]));
        assert!(run_test_test(2, &[" 2x", "-eq", "2"]));
        assert!(run_test_test(2, &["", "-eq", "0"]));
        assert!(run_test_test(2, &["", "-ne", "0"]));
        assert!(run_test_test(2, &["  ", "-eq", "0"]));
        assert!(run_test_test(2, &["  ", "-ne", "0"]));
        assert!(run_test_test(2, &["x", "-eq", "0"]));
        assert!(run_test_test(2, &["x", "-ne", "0"]));
        assert!(run_test_test(1, &["-1", "-ne", "-1"]));
        assert!(run_test_test(0, &["abc", "!=", "def"]));
        assert!(run_test_test(1, &["abc", "=", "def"]));
        assert!(run_test_test(0, &["5", "-le", "10"]));
        assert!(run_test_test(0, &["10", "-le", "10"]));
        assert!(run_test_test(1, &["20", "-le", "10"]));
        assert!(run_test_test(0, &["-1", "-le", "0"]));
        assert!(run_test_test(1, &["0", "-le", "-1"]));
        assert!(run_test_test(0, &["15", "-ge", "10"]));
        assert!(run_test_test(0, &["15", "-ge", "10"]));
        assert!(run_test_test(1, &["!", "15", "-ge", "10"]));
        assert!(run_test_test(0, &["!", "!", "15", "-ge", "10"]));

        assert!(run_test_test(0, &[
            "(", "-d", "/", ")",
            "-o",
            "(", "!", "-d", "/", ")",
        ]));

        assert!(run_test_test(0, &["0", "-ne", "1", "-a", "0", "-eq", "0"]));
        assert!(run_test_test(0, &["0", "-ne", "1", "-a", "-n", "5"]));
        assert!(run_test_test(0, &["-n", "5", "-a", "10", "-gt", "5"]));
        assert!(run_test_test(0, &["-n", "3", "-a", "-n", "5"]));

        // Test precedence:
        //      '0 == 0 || 0 == 1 && 0 == 2'
        //  should be evaluated as:
        //      '0 == 0 || (0 == 1 && 0 == 2)'
        //  and therefore true. If it were
        //      '(0 == 0 || 0 == 1) && 0 == 2'
        //  it would be false.
        assert!(run_test_test(0, &["0", "=", "0", "-o", "0", "=", "1", "-a", "0", "=", "2"]));
        assert!(run_test_test(0, &["-n", "5", "-o", "0", "=", "1", "-a", "0", "=", "2"]));
        assert!(run_test_test(1, &["(", "0", "=", "0", "-o", "0", "=", "1", ")", "-a", "0", "=", "2"]));
        assert!(run_test_test(0, &["0", "=", "0", "-o", "(", "0", "=", "1", "-a", "0", "=", "2", ")"]));

        // A few lame tests for permissions; these need to be a lot more complete.
        // NOTE: we assume /bin/sh exists and is executable here
        assert!(run_test_test(0, &["-e", "/bin/sh"]));
        assert!(run_test_test(1, &["-e", "/bin/sh_not_a_path"]));
        assert!(run_test_test(0, &["-x", "/bin/sh"]));
        assert!(run_test_test(1, &["-x", "/bin/sh_not_a_path"]));
        assert!(run_test_test(0, &["-d", "/bin/"]));
        assert!(run_test_test(1, &["-d", "/bin/sh"]));

        // This failed at one point.
        assert!(run_test_test(1, &["-d", "/bin", "-a", "5", "-eq", "3"]));
        assert!(run_test_test(0, &["-d", "/bin", "-o", "5", "-eq", "3"]));
        assert!(run_test_test(0,&["-d", "/bin", "-a", "!", "5", "-eq", "3"]));

        // We didn't properly handle multiple "just strings" either.
        assert!(run_test_test(0, &["foo"]));
        assert!(run_test_test(0, &["foo", "-a", "bar"]));

        // These should be errors.
        assert!(run_test_test(1, &["foo", "bar"]));
        assert!(run_test_test(1, &["foo", "bar", "baz"]));

        // This crashed.
        assert!(run_test_test(1, &["1", "=", "1", "-a", "=", "1"]));

        // Make sure we can treat -S as a parameter instead of an operator.
        // https://github.com/fish-shell/fish-shell/issues/601
        assert!(run_test_test(0, &["-S", "=", "-S"]));
        assert!(run_test_test(1, &["!", "!", "!", "A"]));

        // Verify that 1. doubles are treated as doubles, and 2. integers that cannot be represented as
        // doubles are still treated as integers.
        assert!(run_test_test(0, &["4611686018427387904", "-eq", "4611686018427387904"]));
        assert!(run_test_test(0, &["4611686018427387904.0", "-eq", "4611686018427387904.0"]));
        assert!(run_test_test(0, &["4611686018427387904.00000000000000001", "-eq", "4611686018427387904.0"]));
        assert!(run_test_test(1, &["4611686018427387904", "-eq", "4611686018427387905"]));
        assert!(run_test_test(0, &["-4611686018427387904", "-ne", "4611686018427387904"]));
        assert!(run_test_test(0, &["-4611686018427387904", "-le", "4611686018427387904"]));
        assert!(run_test_test(1, &["-4611686018427387904", "-ge", "4611686018427387904"]));
        assert!(run_test_test(1, &["4611686018427387904", "-gt", "4611686018427387904"]));
        assert!(run_test_test(0, &["4611686018427387904", "-ge", "4611686018427387904"]));

        // test out-of-range numbers
        assert!(run_test_test(2, &["99999999999999999999999999", "-ge", "1"]));
        assert!(run_test_test(2, &["1", "-eq", "-99999999999999999999999999.9"]));
    }

    #[test]
    #[serial]
    fn test_test_builtin() {
        test_init();
        test_test_brackets();
        test_test();
    }
}
