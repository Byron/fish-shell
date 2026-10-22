use crate::string_regex::{MatchOptions, StringMatcher};
use std::num::NonZeroUsize;

use super::*;
use crate::{env::EnvVar, flog::flog, parser::ParserEnvSetMode};

#[derive(Default)]
pub struct Match<'args> {
    all: bool,
    entire: bool,
    groups_only: bool,
    ignore_case: bool,
    invert_match: bool,
    quiet: bool,
    regex: bool,
    index: bool,
    pattern: &'args wstr,
    max_matches: Option<NonZeroUsize>,
}

impl<'args> StringSubCommand<'args> for Match<'args> {
    const LONG_OPTIONS: &'static [WOption<'static>] = &[
        wopt(L!("all"), NoArgument, 'a'),
        wopt(L!("entire"), NoArgument, 'e'),
        wopt(L!("groups-only"), NoArgument, 'g'),
        wopt(L!("ignore-case"), NoArgument, 'i'),
        wopt(L!("invert"), NoArgument, 'v'),
        wopt(L!("quiet"), NoArgument, 'q'),
        wopt(L!("regex"), NoArgument, 'r'),
        wopt(L!("index"), NoArgument, 'n'),
        wopt(L!("max-matches"), RequiredArgument, 'm'),
    ];
    const SHORT_OPTIONS: &'static wstr = L!("aegivqrnm:");

    fn parse_opt(&mut self, c: char, arg: Option<&wstr>) -> Result<(), StringError<'_>> {
        match c {
            'a' => self.all = true,
            'e' => self.entire = true,
            'g' => self.groups_only = true,
            'i' => self.ignore_case = true,
            'v' => self.invert_match = true,
            'q' => self.quiet = true,
            'r' => self.regex = true,
            'n' => self.index = true,
            'm' => {
                self.max_matches = {
                    let arg = arg.expect("Option -m requires a non-zero argument");
                    let max = fish_wcstoul(arg)
                        .ok()
                        .and_then(|v| NonZeroUsize::new(v as usize))
                        .ok_or_else(|| {
                            StringError::InvalidArgs(err_fmt!(Error::INVALID_MAX_MATCHES, arg))
                        })?;
                    Some(max)
                }
            }
            _ => return Err(StringError::UnknownOption),
        }
        Ok(())
    }

    fn take_args(
        &mut self,
        optind: &mut usize,
        args: &[&'args wstr],
        streams: &mut IoStreams,
    ) -> Result<(), ErrorCode> {
        let cmd = L!("string");
        let subcmd = args[0];
        let Some(arg) = args.get(*optind).copied() else {
            err_fmt!(Error::MISSING_ARG)
                .subcmd(cmd, subcmd)
                .finish(streams);
            return Err(STATUS_INVALID_ARGS);
        };
        *optind += 1;
        self.pattern = arg;
        Ok(())
    }

    fn handle(
        &mut self,
        parser: &mut Parser,
        streams: &mut IoStreams,
        optind: &mut usize,
        args: &[&wstr],
    ) -> Result<(), ErrorCode> {
        let cmd = L!("string");
        let subcmd = args[0];

        if self.entire && self.index {
            err_fmt!(
                Error::INVALID_OPT_COMBO_WITH_CTX,
                wgettext!("--entire and --index are mutually exclusive")
            )
            .subcmd(cmd, subcmd)
            .finish(streams);
            return Err(STATUS_INVALID_ARGS);
        }

        if self.invert_match && self.groups_only {
            err_fmt!(
                Error::INVALID_OPT_COMBO_WITH_CTX,
                wgettext!("--invert and --groups-only are mutually exclusive")
            )
            .subcmd(cmd, subcmd)
            .finish(streams);
            return Err(STATUS_INVALID_ARGS);
        }

        if self.entire && self.groups_only {
            err_fmt!(
                Error::INVALID_OPT_COMBO_WITH_CTX,
                wgettext!("--entire and --groups-only are mutually exclusive")
            )
            .subcmd(cmd, subcmd)
            .finish(streams);
            return Err(STATUS_INVALID_ARGS);
        }

        let mut matcher = match StringMatcher::new(
            self.pattern,
            MatchOptions {
                all: self.all,
                entire: self.entire,
                groups_only: self.groups_only,
                ignore_case: self.ignore_case,
                invert_match: self.invert_match,
                quiet: self.quiet,
                index: self.index,
                regex: self.regex,
            },
            |name| EnvVar::flags_for(name).read_only,
        ) {
            Ok(m) => m,
            Err(e) => {
                e.print_error(args, streams);
                return Err(STATUS_INVALID_ARGS);
            }
        };

        for InputValue { arg, .. } in arguments(args, optind, streams) {
            if let Err(e) = matcher.report_matches(arg.as_ref(), |line| {
                streams.out.appendln(line);
            }) {
                flog!(error, "pcre2_match unexpected error:", e.error_message());
            }
            let match_count = matcher.match_count();
            if self.quiet && match_count > 0
                || self.max_matches.is_some_and(|m| m.get() == match_count)
            {
                break;
            }
        }

        let match_count = matcher.match_count();
        for (name, vals) in matcher.named_captures() {
            parser.set_var(&WString::from(name), ParserEnvSetMode::default(), vals);
        }

        if match_count > 0 {
            Ok(())
        } else {
            Err(STATUS_CMD_ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::builtins::{STATUS_CMD_ERROR, STATUS_CMD_OK};
    use crate::tests::prelude::*;
    use crate::validate;
    use fish_feature_flags::{FeatureFlag, with_overridden_feature};

    #[test]
    #[serial]
    #[rustfmt::skip]
    fn plain() {
        test_init();
        super::super::test_helpers::cases::match_cases(super::super::test_helpers::validate_case);
    }

    #[test]
    #[serial]
    #[rustfmt::skip]
    fn test_qmark_noglob_true() {
        with_overridden_feature(FeatureFlag::QuestionMarkNoGlob, true, || {
            validate!(["string", "match", "a*b?c", "axxb?c"], STATUS_CMD_OK, "axxb?c\n");
            validate!(["string", "match", "*?", "a"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "*?", "ab"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "?*", "a"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "?*", "ab"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "a*\\?", "abc?"], STATUS_CMD_ERROR, "");

            validate!(["string", "match", "?", "?"], STATUS_CMD_OK, "?\n");
            validate!(["string", "match", "a??b", "axxb"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "a??b", "a??b"], STATUS_CMD_OK, "a??b\n");
            validate!(["string", "match", "-i", "a??B", "axxb"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "-i", "a??b", "A??b"], STATUS_CMD_OK, "A??b\n");
            validate!(["string", "match", "a*\\?", "abc\\?"], STATUS_CMD_OK, "abc\\?\n");

            validate!(["string", "match", "?", ""], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "?", "ab"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "??", "a"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "?a", "a"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "a?", "a"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "a??B", "axxb"], STATUS_CMD_ERROR, "");
        });
    }

    #[test]
    #[serial]
    #[rustfmt::skip]
    fn test_qmark_glob() {
        with_overridden_feature(FeatureFlag::QuestionMarkNoGlob, false, || {
            validate!(["string", "match", "a*b?c", "axxbyc"], STATUS_CMD_OK, "axxbyc\n");
            validate!(["string", "match", "*?", "a"], STATUS_CMD_OK, "a\n");
            validate!(["string", "match", "*?", "ab"], STATUS_CMD_OK, "ab\n");
            validate!(["string", "match", "?*", "a"], STATUS_CMD_OK, "a\n");
            validate!(["string", "match", "?*", "ab"], STATUS_CMD_OK, "ab\n");
            validate!(["string", "match", "a*\\?", "abc?"], STATUS_CMD_OK, "abc?\n");

            validate!(["string", "match", "?", ""], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "?", "ab"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "??", "a"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "?a", "a"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "a?", "a"], STATUS_CMD_ERROR, "");
            validate!(["string", "match", "a??B", "axxb"], STATUS_CMD_ERROR, "");
        });
    }
}
