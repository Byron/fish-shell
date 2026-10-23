//! POSIX scopes, diagnostics and validation for the shared argparse parser.
use super::prelude::*;
use crate::argparse::{self, Context, ParseError};
use crate::builtins::Error;
use crate::env::EnvMode;
use crate::exec::exec_subshell;
use crate::parser::ParserEnvSetMode;
use crate::{err_fmt, err_str};
use fish_fluent::localize_fn;

localizable_consts!(
    MISSING_DOUBLE_HYPHEN_SEPARATOR
    "Missing -- separator"
);

localize_fn! {
    err_invalid_opt_spec,
    "argparse-invalid-option-spec" =
        "Invalid option spec '{ $option_spec }' at char '{ $bad_char }'",
    option_spec,
    bad_char,
}

struct PosixContext<'a, 'b, 'c> {
    parser: &'a mut Parser,
    streams: &'b mut IoStreams<'c>,
}

impl Context for PosixContext<'_, '_, '_> {
    type Error = ErrorCode;

    fn error(&mut self, name: &wstr, error: ParseError<'_>, status: i32) -> ErrorCode {
        let streams = &mut *self.streams;
        match error {
            ParseError::MissingSeparator => err_str!(MISSING_DOUBLE_HYPHEN_SEPARATOR)
                .cmd(name)
                .finish(streams),
            ParseError::Exclusive(a, b) => err_fmt!(Error::COMBO_EXCLUSIVE, a, b)
                .cmd(name)
                .finish(streams),
            ParseError::InvalidExclusiveSet(flags) => Error::from(&localize!(
                "argparse-exclusive-flag-string-invalid" =
                    "exclusive flag string '{ $flag_string }' is not valid",
                flag_string = flags,
            ))
            .cmd(name)
            .finish(streams),
            ParseError::InvalidExclusiveFlag(flag) => Error::from(&localize!(
                "argparse-exclusive-flag-invalid" = "exclusive flag '{ $flag }' is not valid",
                flag = flag,
            ))
            .cmd(name)
            .finish(streams),
            ParseError::ImplicitIntModifier(flag, modifier) => err_fmt!(
                "Implicit int short flag '%c' does not allow modifiers like '%c'",
                flag,
                modifier
            )
            .cmd(name)
            .finish(streams),
            ParseError::InvalidOptionSpec(spec, c) => Error::from(&err_invalid_opt_spec(spec, c))
                .cmd(name)
                .finish(streams),
            ParseError::DuplicateShort(flag) => err_fmt!("Short flag '%c' already defined", flag)
                .cmd(name)
                .finish(streams),
            ParseError::DuplicateImplicitInt(flag) => {
                localize_fn! {
                    localize_implicit_int_flag_already_defined,
                    "argparse-implicit-int-flag-already-defined" =
                        "Implicit int flag '{ $flag }' already defined",
                    flag,
                }
                Error::from(&localize_implicit_int_flag_already_defined(flag))
                    .cmd(name)
                    .finish(streams);
            }
            ParseError::EmptyOptionSpec => {
                err_str!("An option spec must have at least a short or a long flag")
                    .cmd(name)
                    .finish(streams);
            }
            ParseError::InvalidShort(flag) => {
                err_fmt!("Short flag '%c' invalid, must be alphanum or '#'", flag)
                    .cmd(name)
                    .finish(streams);
            }
            ParseError::DuplicateLong(flag) => err_fmt!("Long flag '%s' already defined", flag)
                .cmd(name)
                .finish(streams),
            ParseError::TooManyLongOptions => err_str!("Too many long-only options")
                .cmd(name)
                .finish(streams),
            ParseError::InvalidUnknownArguments(value) => {
                err_fmt!("Invalid --unknown-arguments value '%s'", value)
                    .cmd(name)
                    .finish(streams);
            }
            ParseError::InvalidMinArgs(value) => err_fmt!("Invalid --min-args value '%s'", value)
                .cmd(name)
                .finish(streams),
            ParseError::InvalidMaxArgs(value) => err_fmt!("Invalid --max-args value '%s'", value)
                .cmd(name)
                .finish(streams),
            ParseError::MissingArgument(option) => {
                builtin_missing_argument(self.parser, streams, name, None, option, false);
            }
            ParseError::MissingUnknownArgument(option) => err_fmt!(Error::MISSING_OPT_ARG, option)
                .cmd(name)
                .finish(streams),
            ParseError::UnexpectedArgument(option) => {
                builtin_unexpected_argument(self.parser, streams, name, option, false);
            }
            ParseError::UnknownOption(option) => {
                builtin_unknown_option(self.parser, streams, name, option, false);
            }
            ParseError::MinArgs(min, actual) => err_fmt!(Error::MIN_ARG_COUNT, min, actual)
                .cmd(name)
                .finish(streams),
            ParseError::MaxArgs(max, actual) => err_fmt!(Error::MAX_ARG_COUNT, max, actual)
                .cmd(name)
                .finish(streams),
        }
        status
    }

    fn usage_error(&mut self, name: &wstr) {
        builtin_print_error_trailer(self.parser, self.streams.err, name);
    }

    fn validate(
        &mut self,
        name: &wstr,
        flag: &wstr,
        value: &wstr,
        command: &wstr,
    ) -> Result<(), ErrorCode> {
        self.parser.vars().push(true /* new_scope */);
        let mode = ParserEnvSetMode::new(EnvMode::LOCAL_EXPORTED);
        self.parser
            .set_one(L!("_argparse_cmd"), mode, name.to_owned());
        self.parser.set_one(L!("_flag_name"), mode, flag.to_owned());
        self.parser
            .set_one(L!("_flag_value"), mode, value.to_owned());
        let mut cmd_output = Vec::new();
        let result = exec_subshell(command, self.parser, Some(&mut cmd_output), false);
        for output in cmd_output {
            self.streams.err.appendln(&output);
        }
        self.parser.vars().pop(self.parser.is_repainting());
        result
    }
}

/// Parse options and install successful results as caller-local variables.
pub fn argparse(parser: &mut Parser, streams: &mut IoStreams, args: &mut [&wstr]) -> BuiltinResult {
    let name = parser
        .get_function_name(1)
        .unwrap_or_else(|| L!("argparse").to_owned());
    let parsed = argparse::parse(args, &name, &mut PosixContext { parser, streams })?;
    if parsed.print_help {
        builtin_print_help(parser, streams, args[0]);
    } else {
        let mode = parser.convert_env_set_mode(ParserEnvSetMode::new(EnvMode::LOCAL));
        for (name, values) in parsed.variables {
            parser.vars().set(&name, mode, values);
        }
    }
    Ok(SUCCESS)
}
