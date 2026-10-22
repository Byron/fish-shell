use std::num::NonZeroUsize;

use crate::string_regex::{ReplaceOptions, StringReplacer};

use super::*;

#[derive(Default)]
pub struct Replace<'args> {
    all: bool,
    filter: bool,
    ignore_case: bool,
    quiet: bool,
    regex: bool,
    pattern: &'args wstr,
    replacement: &'args wstr,
    max_matches: Option<NonZeroUsize>,
}

impl<'args> StringSubCommand<'args> for Replace<'args> {
    const LONG_OPTIONS: &'static [WOption<'static>] = &[
        wopt(L!("all"), NoArgument, 'a'),
        wopt(L!("filter"), NoArgument, 'f'),
        wopt(L!("ignore-case"), NoArgument, 'i'),
        wopt(L!("quiet"), NoArgument, 'q'),
        wopt(L!("regex"), NoArgument, 'r'),
        wopt(L!("max-matches"), RequiredArgument, 'm'),
    ];
    const SHORT_OPTIONS: &'static wstr = L!("afiqrm:");

    fn parse_opt(&mut self, c: char, arg: Option<&wstr>) -> Result<(), StringError<'_>> {
        match c {
            'a' => self.all = true,
            'f' => self.filter = true,
            'i' => self.ignore_case = true,
            'q' => self.quiet = true,
            'r' => self.regex = true,
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
        let Some(pattern) = args.get(*optind).copied() else {
            err_str!(Error::MISSING_ARG)
                .subcmd(cmd, subcmd)
                .finish(streams);
            return Err(STATUS_INVALID_ARGS);
        };
        *optind += 1;
        let Some(replacement) = args.get(*optind).copied() else {
            err_fmt!(Error::UNEXP_ARG_COUNT, 2, 1)
                .subcmd(cmd, subcmd)
                .finish(streams);
            return Err(STATUS_INVALID_ARGS);
        };
        *optind += 1;

        self.pattern = pattern;
        self.replacement = replacement;
        Ok(())
    }

    fn handle(
        &mut self,
        _parser: &mut Parser,
        streams: &mut IoStreams,
        optind: &mut usize,
        args: &[&wstr],
    ) -> Result<(), ErrorCode> {
        let cmd = L!("string");
        let subcmd = args[0];

        let replacer = match StringReplacer::new(
            self.pattern,
            self.replacement,
            ReplaceOptions {
                all: self.all,
                ignore_case: self.ignore_case,
                regex: self.regex,
            },
        ) {
            Ok(x) => x,
            Err(e) => {
                e.print_error(args, streams);
                return Err(STATUS_INVALID_ARGS);
            }
        };

        let mut replace_count = 0;

        for InputValue { arg, want_newline } in arguments(args, optind, streams) {
            let (replaced, result) = match replacer.replace(arg) {
                Ok(x) => x,
                Err(e) => {
                    err_fmt!("Regular expression substitute error: %s", e.error_message())
                        .subcmd(cmd, subcmd)
                        .finish(streams);
                    return Err(STATUS_INVALID_ARGS);
                }
            };
            replace_count += replaced as usize;

            if !self.quiet && (!self.filter || replaced) {
                streams.out.append(&result);
                if want_newline {
                    streams.out.append('\n');
                }
            }

            if self.quiet && replace_count > 0 {
                return Ok(());
            }
            if self
                .max_matches
                .is_some_and(|max| max.get() == replace_count)
            {
                return Ok(());
            }
        }

        if replace_count > 0 {
            Ok(())
        } else {
            Err(STATUS_CMD_ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::prelude::*;

    #[test]
    #[serial]
    #[rustfmt::skip]
    fn plain() {
        test_init();
        super::super::test_helpers::cases::replace_cases(super::super::test_helpers::validate_case);
    }
}
