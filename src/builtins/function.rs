use super::prelude::*;
use crate::ast::BlockStatement;
use crate::common::valid_var_name;
use crate::complete::complete_add_wrapper;
use crate::env::Environment as _;
use crate::env::is_read_only;
use crate::event::{self, EventDescription, EventHandler};
use crate::function_options::{self, ParseError, valid_function_name};
use crate::global_safety::RelaxedAtomicBool;
use crate::parse_execution::varname_error;
use crate::parse_tree::NodeRef;
use crate::parser_keywords::parser_keywords_is_reserved;
use crate::proc::{InternalJobId, Pid};
use crate::signal::RawSignal;
use crate::{err_fmt, err_str, function};
use nix::unistd::getpid;
use std::sync::Arc;

/// Return the internal_job_id for a pid, or None if none.
/// This looks through both active and finished jobs.
fn job_id_for_pid(pid: Pid, parser: &Parser) -> Option<InternalJobId> {
    if let Some(job) = parser.job_get_from_pid(pid) {
        Some(job.internal_job_id)
    } else {
        parser
            .wait_handles()
            .get_by_pid(pid)
            .map(|h| h.internal_job_id)
    }
}

/// Preserve POSIX event lookup and diagnostics while sharing the option grammar.
struct OptionContext<'a, 'b> {
    parser: &'a Parser,
    streams: &'a mut IoStreams<'b>,
}

impl function_options::Context for OptionContext<'_, '_> {
    type Event = EventDescription;
    type Error = ErrorCode;

    fn validate_variable(&mut self, name: &wstr, read_only_ok: bool) -> Result<(), ErrorCode> {
        let cmd = L!("function");
        if !valid_var_name(name) {
            varname_error(cmd, name).finish(self.streams);
            return Err(STATUS_INVALID_ARGS);
        }
        if !read_only_ok && is_read_only(name) {
            err_fmt!("variable '%s' is read-only", name)
                .cmd(cmd)
                .finish(self.streams);
            return Err(STATUS_INVALID_ARGS);
        }
        Ok(())
    }

    fn event(&mut self, option: char, value: &wstr) -> Result<EventDescription, ErrorCode> {
        let cmd = L!("function");
        let parser = self.parser;
        let streams = &mut *self.streams;
        Ok(match option {
            's' => {
                let Some(signal) = RawSignal::parse(value) else {
                    err_fmt!("Unknown signal '%s'", value)
                        .cmd(cmd)
                        .finish(streams);
                    return Err(STATUS_INVALID_ARGS);
                };
                EventDescription::Signal { signal }
            }
            'v' => EventDescription::Variable {
                name: value.to_owned(),
            },
            'e' => EventDescription::Generic {
                param: value.to_owned(),
            },
            'j' if value == "caller" => {
                let caller_id = if parser.scope().is_subshell {
                    parser.scope().caller_id
                } else {
                    InternalJobId::default()
                };
                if caller_id == InternalJobId::default() {
                    err_str!("calling job for event handler not found")
                        .cmd(cmd)
                        .finish(streams);
                    return Err(STATUS_INVALID_ARGS);
                }
                EventDescription::CallerExit { caller_id }
            }
            'p' if value == "%self" => EventDescription::ProcessExit {
                pid: Some(Pid::from_nullable_pid_unchecked(getpid())),
            },
            'j' | 'p' => {
                let pid = parse_pid_may_be_zero(streams, cmd, value)?;
                if option == 'p' {
                    EventDescription::ProcessExit { pid }
                } else {
                    // TODO: rationalize why a default of 0 is sensible.
                    let internal_job_id = pid
                        .and_then(|pid| job_id_for_pid(pid, parser))
                        .unwrap_or_default();
                    EventDescription::JobExit {
                        pid,
                        internal_job_id,
                    }
                }
            }
            _ => unreachable!("unexpected function event option"),
        })
    }

    fn error(&mut self, error: ParseError<'_>) -> ErrorCode {
        let cmd = L!("function");
        let print_hints = false;
        match error {
            ParseError::UnexpectedPositional(value) => {
                err_fmt!("%s: unexpected positional argument", value)
                    .cmd(cmd)
                    .finish(self.streams);
            }
            ParseError::MissingArgument(value) => {
                builtin_missing_argument(self.parser, self.streams, cmd, None, value, print_hints);
            }
            ParseError::UnexpectedArgument(value) => {
                builtin_unexpected_argument(self.parser, self.streams, cmd, value, print_hints);
            }
            ParseError::UnknownOption(value) => {
                builtin_unknown_option(self.parser, self.streams, cmd, value, print_hints);
            }
        }
        STATUS_INVALID_ARGS
    }
}

fn validate_function_name(
    argv: &mut [&wstr],
    function_name: &mut WString,
    cmd: &wstr,
    streams: &mut IoStreams,
) -> BuiltinResult {
    if argv.len() < 2 {
        streams
            .err
            .append(&wgettext_fmt!("%s: function name required", cmd));
        return Err(STATUS_INVALID_ARGS);
    }
    *function_name = argv[1].to_owned();
    if !valid_function_name(function_name) {
        err_fmt!("%s: invalid function name", function_name,)
            .cmd(cmd)
            .finish(streams);
        return Err(STATUS_INVALID_ARGS);
    }
    if parser_keywords_is_reserved(function_name) {
        err_fmt!(
            "%s: cannot use reserved keyword as function name",
            function_name
        )
        .cmd(cmd)
        .finish(streams);
        return Err(STATUS_INVALID_ARGS);
    }
    Ok(SUCCESS)
}

/// Define a function. Calls into `function.rs` to perform the heavy lifting of defining a
/// function. Note this isn't strictly a "builtin": it is called directly from parse_execution.
/// That is why its signature is different from the other builtins.
pub fn function(
    parser: &mut Parser,
    streams: &mut IoStreams,
    c_args: &mut [&wstr],
    func_node: NodeRef<BlockStatement>,
) -> BuiltinResult {
    // The wgetopt function expects 'function' as the first argument. Make a new vec with
    // that property. This is needed because this builtin has a different signature than the other
    // builtins.
    let mut args = vec![L!("function")];
    args.extend_from_slice(c_args);
    let argv: &mut [&wstr] = &mut args;
    let cmd = argv[0];

    // A valid function name has to be the first argument.
    let mut function_name = WString::new();
    validate_function_name(argv, &mut function_name, cmd, streams)?;
    let argv = &mut argv[1..];

    let mut opts = function_options::parse(argv, &mut OptionContext { parser, streams })?;

    if opts.print_help {
        builtin_print_error_trailer(parser, streams.err, cmd);
        return Ok(SUCCESS);
    }

    // Extract the current filename.
    let definition_file = parser.current_filename();

    // Ensure inherit_vars is unique and then populate it.
    opts.inherit_vars.sort_unstable();
    opts.inherit_vars.dedup();

    let inherit_vars: Vec<(WString, Vec<WString>)> = opts
        .inherit_vars
        .into_iter()
        .filter_map(|name| {
            let vals = parser.vars().get(&name)?.as_list().to_vec();
            Some((name, vals))
        })
        .collect();

    // We have what we need to actually define the function.
    let props = function::FunctionProperties {
        func_node,
        named_arguments: opts.named_arguments,
        // Function descriptions are extracted from scripts in `share` via
        // `cargo xtask gettext update`.
        description: LocalizableString::from_external_source(opts.description),
        inherit_vars: inherit_vars.into_boxed_slice(),
        shadow_scope: opts.shadow_scope,
        is_autoload: RelaxedAtomicBool::new(false),
        definition_file,
        copy_definition_lineno: None,
    };

    // Add the function itself.
    function::add(function_name.clone(), Arc::new(props));

    // Handle wrap targets by creating the appropriate completions.
    for wt in opts.wrap_targets {
        complete_add_wrapper(function_name.clone(), wt);
    }

    // Add any event handlers.
    for ed in &opts.events {
        event::add_handler(EventHandler::new(ed.clone(), Some(function_name.clone())));
    }

    // If there is an --on-process-exit or --on-job-exit event handler for some pid, and that
    // process has already exited, run it immediately (#7210).
    for ed in &opts.events {
        match *ed {
            EventDescription::ProcessExit { pid: Some(pid) } => {
                let wh = parser.wait_handles().get_by_pid(pid);
                if let Some(status) = wh.and_then(|wh| wh.status()) {
                    event::fire(parser, event::Event::process_exit(pid, status));
                }
            }
            EventDescription::JobExit { pid: Some(pid), .. } => {
                let wh = parser.wait_handles().get_by_pid(pid);
                if let Some(wh) = wh {
                    if wh.is_completed() {
                        event::fire(parser, event::Event::job_exit(pid, wh.internal_job_id));
                    }
                }
            }
            _ => (),
        }
    }

    Ok(SUCCESS)
}
