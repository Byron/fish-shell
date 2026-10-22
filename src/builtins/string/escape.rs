use super::*;
use fish_common::{EscapeFlags, EscapeStringStyle, escape_string};

#[derive(Default)]
pub struct Escape {
    no_quoted: bool,
    style: EscapeStringStyle,
}

impl StringSubCommand<'_> for Escape {
    const LONG_OPTIONS: &'static [WOption<'static>] = &[
        wopt(L!("no-quoted"), NoArgument, 'n'),
        wopt(L!("style"), RequiredArgument, NON_OPTION_CHAR),
    ];
    const SHORT_OPTIONS: &'static wstr = L!("n");

    fn parse_opt(&mut self, c: char, arg: Option<&wstr>) -> Result<(), StringError<'_>> {
        match c {
            'n' => self.no_quoted = true,
            NON_OPTION_CHAR => {
                self.style = arg
                    .unwrap()
                    .try_into()
                    .map_err(|_| err_fmt!("Invalid escape style '%s'", arg.unwrap()))?;
            }
            _ => return Err(StringError::UnknownOption),
        }
        Ok(())
    }

    fn handle(
        &mut self,
        _parser: &mut Parser,
        streams: &mut IoStreams,
        optind: &mut usize,
        args: &[&wstr],
    ) -> Result<(), ErrorCode> {
        // Currently, only the script style supports options.
        // Ignore them for other styles for now.
        let style = match self.style {
            EscapeStringStyle::Script(..) if self.no_quoted => {
                EscapeStringStyle::Script(EscapeFlags {
                    no_quoted: true,
                    ..Default::default()
                })
            }
            x => x,
        };

        let mut escaped_any = false;
        for InputValue { arg, want_newline } in arguments(args, optind, streams) {
            let mut escaped = escape_string(&arg, style);

            if want_newline {
                escaped.push('\n');
            }

            streams.out.append(&escaped);
            escaped_any = true;
        }

        if escaped_any {
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
        super::super::test_helpers::cases::escape_cases(super::super::test_helpers::validate_case);
    }
}
