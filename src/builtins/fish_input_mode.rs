use super::prelude::*;

/// Request a language for the next outer interactive input, without changing Fish code.
pub fn fish_input_mode(
    parser: &mut Parser,
    streams: &mut IoStreams,
    args: &mut [&wstr],
) -> BuiltinResult {
    if args.len() == 2 && [L!("-h"), L!("--help")].contains(&args[1]) {
        builtin_print_help(parser, streams, args[0]);
        return Ok(SUCCESS);
    }
    let language = match args {
        [_, language] => *language,
        _ => L!(""),
    };
    if ![L!("fish"), L!("bash")].contains(&language) {
        streams
            .err
            .append(L!("fish_input_mode: expected fish|bash\n"));
        return Err(STATUS_INVALID_ARGS);
    }
    if parser.libdata().rash_shell {
        // Reuse the user's Fish UI configuration without changing the dedicated evaluator.
        if parser.libdata().within_fish_init {
            return Ok(SUCCESS);
        }
        streams.err.append(L!(
            "fish_input_mode: unavailable in a Rash shell; use exit to return to the parent shell\n"
        ));
        return Err(STATUS_CMD_ERROR);
    }
    if !crate::reader::reader_can_change_input_mode(parser) {
        streams.err.append(L!("fish_input_mode: use interactive startup configuration or a submitted command, outside readers, keybindings, hooks and substitutions\n"));
        return Err(STATUS_CMD_ERROR);
    }
    parser.libdata_mut().bash_input_requested = language == "bash";
    Ok(SUCCESS)
}
