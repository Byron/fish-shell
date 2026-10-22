use super::prelude::*;

use crate::builtins::Error;
use crate::random_core::{self, ParseError, Request, Value};
use crate::{err_fmt, err_str};
use std::sync::{LazyLock, Mutex};

static RNG: LazyLock<Mutex<random_core::Engine>> =
    LazyLock::new(|| Mutex::new(random_core::fresh()));

pub fn random(parser: &mut Parser, streams: &mut IoStreams, argv: &mut [&wstr]) -> BuiltinResult {
    let cmd = argv[0];
    let request = match random_core::parse(argv) {
        Ok(request) => request,
        Err(error) => {
            match error {
                ParseError::MissingOptionArgument(arg) => {
                    builtin_missing_argument(parser, streams, cmd, None, arg, false);
                }
                ParseError::UnexpectedOptionArgument(arg) => {
                    builtin_unexpected_argument(parser, streams, cmd, arg, false);
                }
                ParseError::UnknownOption(arg) => {
                    builtin_unknown_option(parser, streams, cmd, arg, false);
                }
                ParseError::InvalidInteger(value) => {
                    err_fmt!(Error::NOT_NUMBER, value).cmd(cmd).finish(streams);
                }
                ParseError::NoChoices => {
                    err_str!("nothing to choose from").cmd(cmd).finish(streams);
                }
                ParseError::ZeroStep => {
                    err_fmt!("%s must be a positive integer", "STEP")
                        .cmd(cmd)
                        .finish(streams);
                }
                ParseError::TooManyArguments => {
                    err_str!(Error::TOO_MANY_ARGUMENTS).cmd(cmd).finish(streams);
                    return Err(STATUS_CMD_ERROR);
                }
            }
            return Err(STATUS_INVALID_ARGS);
        }
    };
    match request {
        Request::Help => builtin_print_help(parser, streams, cmd),
        Request::Seed(seed) => *RNG.lock().unwrap() = random_core::seeded(seed),
        Request::Draw(draw) => {
            let value = draw.sample(&mut RNG.lock().unwrap());
            match value {
                Value::Number(value) => streams.out.appendln(&value.to_wstring()),
                Value::Argument(index) => streams.out.appendln(argv[index]),
            };
        }
    }
    Ok(SUCCESS)
}
