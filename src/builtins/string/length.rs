use super::*;

#[derive(Default)]
pub struct Length {
    quiet: bool,
    visible: bool,
}

impl StringSubCommand<'_> for Length {
    const LONG_OPTIONS: &'static [WOption<'static>] = &[
        wopt(L!("quiet"), NoArgument, 'q'),
        wopt(L!("visible"), NoArgument, 'V'),
    ];
    const SHORT_OPTIONS: &'static wstr = L!("qV");

    fn parse_opt(&mut self, c: char, _arg: Option<&wstr>) -> Result<(), StringError<'_>> {
        match c {
            'q' => self.quiet = true,
            'V' => self.visible = true,
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
        let mut nnonempty = 0usize;

        for InputValue { arg, .. } in arguments(args, optind, streams) {
            if self.visible {
                for max in crate::string_display::WidthSettings::default().line_widths(&arg) {
                    if max > 0 {
                        nnonempty += 1;
                    }
                    if !self.quiet {
                        streams.out.appendln(&max.to_wstring());
                    } else if nnonempty > 0 {
                        return Ok(());
                    }
                }
            } else {
                let n = arg.len();
                if n > 0 {
                    nnonempty += 1;
                }
                if !self.quiet {
                    streams.out.appendln(&n.to_wstring());
                } else if nnonempty > 0 {
                    return Ok(());
                }
            }
        }
        if nnonempty > 0 {
            Ok(())
        } else {
            Err(STATUS_CMD_ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    mod display_cases {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/string_display_cases.rs"
        ));
    }
    use crate::builtins::{STATUS_CMD_ERROR, STATUS_CMD_OK};
    use crate::tests::prelude::*;
    use crate::validate;

    #[test]
    #[serial]
    fn shared_display_cases() {
        test_init();
        display_cases::check(super::super::test_helpers::validate_case);
    }

    #[test]
    #[serial]
    #[rustfmt::skip]
    fn plain() {
        test_init();
        validate!(["string", "length"], STATUS_CMD_ERROR, "");
        validate!(["string", "length", ""], STATUS_CMD_ERROR, "0\n");
        validate!(["string", "length", "", "", ""], STATUS_CMD_ERROR, "0\n0\n0\n");
        validate!(["string", "length", "a"], STATUS_CMD_OK, "1\n");

        validate!(["string", "length", "\u{2008A}"], STATUS_CMD_OK, "1\n");
        validate!(["string", "length", "um", "dois", "três"], STATUS_CMD_OK, "2\n4\n4\n");
        validate!(["string", "length", "um", "dois", "três"], STATUS_CMD_OK, "2\n4\n4\n");
        validate!(["string", "length", "-q"], STATUS_CMD_ERROR, "");
        validate!(["string", "length", "-q", ""], STATUS_CMD_ERROR, "");
        validate!(["string", "length", "-q", "a"], STATUS_CMD_OK, "");
    }
}
