//! Function declaration options shared by the POSIX and native evaluators.
//!
//! Option ordering belongs to the language. Variable validation, event lookup,
//! and diagnostics depend on the evaluator and are supplied by its context.
use fish_wgetopt::{ArgType, NON_OPTION_CHAR, WGetopter, WOption, wopt};
use fish_widestring::{L, WExt as _, WString, wstr};

pub(crate) struct Options<Event> {
    pub print_help: bool,
    pub shadow_scope: bool,
    pub description: WString,
    pub events: Vec<Event>,
    pub named_arguments: Vec<WString>,
    pub inherit_vars: Vec<WString>,
    pub wrap_targets: Vec<WString>,
}

impl<Event> Default for Options<Event> {
    fn default() -> Self {
        Self {
            print_help: false,
            shadow_scope: true,
            description: WString::new(),
            events: Vec::new(),
            named_arguments: Vec::new(),
            inherit_vars: Vec::new(),
            wrap_targets: Vec::new(),
        }
    }
}

pub(crate) enum ParseError<'a> {
    UnexpectedPositional(&'a wstr),
    MissingArgument(&'a wstr),
    UnexpectedArgument(&'a wstr),
    UnknownOption(&'a wstr),
}

pub(crate) trait Context {
    type Event;
    type Error;

    fn validate_variable(&mut self, name: &wstr, read_only_ok: bool) -> Result<(), Self::Error>;
    fn event(&mut self, option: char, value: &wstr) -> Result<Self::Event, Self::Error>;
    fn error(&mut self, error: ParseError<'_>) -> Self::Error;
}

// RETURN_IN_ORDER lets -a consume following positional arguments until another
// option appears. Keep the same getopt grammar for every evaluator.
const SHORT_OPTIONS: &wstr = L!("-a:d:e:hj:p:s:v:w:SV:");
#[rustfmt::skip]
const LONG_OPTIONS: &[WOption] = &[
    wopt(L!("description"), ArgType::RequiredArgument, 'd'),
    wopt(L!("on-signal"), ArgType::RequiredArgument, 's'),
    wopt(L!("on-job-exit"), ArgType::RequiredArgument, 'j'),
    wopt(L!("on-process-exit"), ArgType::RequiredArgument, 'p'),
    wopt(L!("on-variable"), ArgType::RequiredArgument, 'v'),
    wopt(L!("on-event"), ArgType::RequiredArgument, 'e'),
    wopt(L!("wraps"), ArgType::RequiredArgument, 'w'),
    wopt(L!("help"), ArgType::NoArgument, 'h'),
    wopt(L!("argument-names"), ArgType::RequiredArgument, 'a'),
    wopt(L!("no-scope-shadowing"), ArgType::NoArgument, 'S'),
    wopt(L!("inherit-variable"), ArgType::RequiredArgument, 'V'),
];

/// Match the function-name rule used by the POSIX function registry. Reserved
/// words are checked separately so callers can retain their distinct diagnostic.
pub(crate) fn valid_function_name(name: &wstr) -> bool {
    !(name.is_empty() || name.starts_with('-') || name.contains('/') || name.contains('\0'))
}

/// `argv[0]` is the already validated function name, as expected by getopt.
pub(crate) fn parse<C: Context>(
    argv: &mut [&wstr],
    context: &mut C,
) -> Result<Options<C::Event>, C::Error> {
    let mut opts = Options::default();
    let mut handling_named_arguments = false;
    let mut w = WGetopter::new(SHORT_OPTIONS, LONG_OPTIONS, argv);
    while let Some(opt) = w.next_opt() {
        if opt != 'a' && opt != NON_OPTION_CHAR {
            handling_named_arguments = false;
        }
        match opt {
            NON_OPTION_CHAR => {
                let value = w.woptarg.unwrap();
                if !handling_named_arguments {
                    return Err(context.error(ParseError::UnexpectedPositional(value)));
                }
                context.validate_variable(value, false)?;
                opts.named_arguments.push(value.to_owned());
            }
            'd' => opts.description = w.woptarg.unwrap().to_owned(),
            's' | 'j' | 'p' | 'v' | 'e' => {
                let value = w.woptarg.unwrap();
                if opt == 'v' {
                    context.validate_variable(value, true)?;
                }
                opts.events.push(context.event(opt, value)?);
            }
            'a' => {
                handling_named_arguments = true;
                let value = w.woptarg.unwrap();
                context.validate_variable(value, false)?;
                opts.named_arguments.push(value.to_owned());
            }
            'S' => opts.shadow_scope = false,
            'w' => opts.wrap_targets.push(w.woptarg.unwrap().to_owned()),
            'V' => {
                let value = w.woptarg.unwrap();
                context.validate_variable(value, false)?;
                opts.inherit_vars.push(value.to_owned());
            }
            'h' => opts.print_help = true,
            ':' | ';' | '?' => {
                let value = w.argv[w.wopt_index - 1];
                return Err(context.error(match opt {
                    ':' => ParseError::MissingArgument(value),
                    ';' => ParseError::UnexpectedArgument(value),
                    _ => ParseError::UnknownOption(value),
                }));
            }
            other => panic!("Unexpected retval from WGetopter: {other}"),
        }
    }
    let optind = w.wopt_index;
    if argv.len() != optind {
        if opts.named_arguments.is_empty() {
            return Err(context.error(ParseError::UnexpectedPositional(argv[optind])));
        }
        for &arg in &argv[optind..] {
            context.validate_variable(arg, false)?;
            opts.named_arguments.push(arg.to_owned());
        }
    }
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestContext {
        variables: Vec<(String, bool)>,
    }

    impl Context for TestContext {
        type Event = (char, String);
        type Error = String;

        fn validate_variable(&mut self, name: &wstr, read_only_ok: bool) -> Result<(), String> {
            self.variables.push((name.to_string(), read_only_ok));
            if name == "status" && !read_only_ok {
                return Err("read-only variable".into());
            }
            Ok(())
        }

        fn event(&mut self, option: char, value: &wstr) -> Result<Self::Event, String> {
            Ok((option, value.to_string()))
        }

        fn error(&mut self, error: ParseError<'_>) -> String {
            match error {
                ParseError::UnexpectedPositional(value) => format!("positional: {value}"),
                ParseError::MissingArgument(value) => format!("missing: {value}"),
                ParseError::UnexpectedArgument(value) => format!("unexpected: {value}"),
                ParseError::UnknownOption(value) => format!("unknown: {value}"),
            }
        }
    }

    fn options(
        args: &[&str],
        context: &mut TestContext,
    ) -> Result<Options<(char, String)>, String> {
        let args: Vec<WString> = args.iter().map(|arg| WString::from(*arg)).collect();
        let mut args: Vec<&wstr> = args.iter().map(|arg| arg.as_ref()).collect();
        parse(&mut args, context)
    }

    #[test]
    fn named_arguments_keep_posix_ordering_around_options_and_terminators() {
        let parsed = options(
            &[
                "name",
                "-a",
                "first",
                "second",
                "-d",
                "description",
                "--",
                "third",
            ],
            &mut TestContext::default(),
        )
        .unwrap();
        assert_eq!(
            parsed.named_arguments,
            [
                WString::from("first"),
                WString::from("second"),
                WString::from("third")
            ]
        );
        assert_eq!(parsed.description, "description");
        assert_eq!(
            options(
                &["name", "-a", "first", "-d", "description", "second"],
                &mut TestContext::default()
            )
            .err(),
            Some("positional: second".into())
        );
        assert_eq!(
            options(&["name", "--", "argument"], &mut TestContext::default()).err(),
            Some("positional: argument".into())
        );
    }

    #[test]
    fn getopt_forms_and_event_adapters_share_the_same_declaration() {
        let mut context = TestContext::default();
        let parsed = options(
            &[
                "name",
                "-dold",
                "--desc=new",
                "-Sw",
                "git log",
                "--wraps=echo",
                "-V",
                "captured",
                "--on-variable",
                "status",
                "--on-event=event",
                "-s",
                "INT",
                "-j",
                "caller",
                "-p",
                "%self",
            ],
            &mut context,
        )
        .unwrap();
        assert_eq!(parsed.description, "new");
        assert!(!parsed.shadow_scope);
        assert_eq!(
            parsed.wrap_targets,
            [WString::from("git log"), WString::from("echo")]
        );
        assert_eq!(parsed.inherit_vars, [WString::from("captured")]);
        assert_eq!(
            context.variables,
            [("captured".into(), false), ("status".into(), true)]
        );
        assert_eq!(
            parsed.events,
            [
                ('v', "status".into()),
                ('e', "event".into()),
                ('s', "INT".into()),
                ('j', "caller".into()),
                ('p', "%self".into())
            ]
        );
    }

    #[test]
    fn validation_errors_stop_before_later_options() {
        let mut context = TestContext::default();
        assert_eq!(
            options(
                &["name", "-a", "first", "status", "--unknown"],
                &mut context
            )
            .err(),
            Some("read-only variable".into())
        );
        assert_eq!(
            context.variables,
            [("first".into(), false), ("status".into(), false)]
        );
        for (args, expected) in [
            (vec!["name", "--description"], "missing: --description"),
            (
                vec!["name", "--no-scope-shadowing=value"],
                "unexpected: --no-scope-shadowing=value",
            ),
            (vec!["name", "--unknown"], "unknown: --unknown"),
        ] {
            assert_eq!(
                options(&args, &mut TestContext::default()).err().as_deref(),
                Some(expected)
            );
        }
    }
}
