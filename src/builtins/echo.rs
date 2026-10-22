//! Implementation of the echo builtin.

use super::prelude::*;

pub fn echo(_parser: &mut Parser, streams: &mut IoStreams, args: &mut [&wstr]) -> BuiltinResult {
    let output = crate::echo::output(args).ok_or(STATUS_INVALID_ARGS)?;
    if !output.is_empty() {
        streams.out.append(&output);
    }
    Ok(SUCCESS)
}
