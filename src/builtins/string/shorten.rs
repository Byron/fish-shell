use super::*;
use fish_widestring::{ELLIPSIS_CHAR, decoded_width};

pub struct Shorten<'args> {
    ellipsis: &'args wstr,
    ellipsis_width: usize,
    max: Option<usize>,
    no_newline: bool,
    quiet: bool,
    shorten_from: Direction,
}
/// The character to use where the text has been truncated, in a [`wstr`].
const ELLIPSIS_WSTR: &wstr = wstr::from_char_slice(&[ELLIPSIS_CHAR]);

impl Default for Shorten<'_> {
    fn default() -> Self {
        Self {
            ellipsis: ELLIPSIS_WSTR,
            ellipsis_width: decoded_width(ELLIPSIS_WSTR),
            max: None,
            no_newline: false,
            quiet: false,
            shorten_from: Direction::Right,
        }
    }
}

impl<'args> StringSubCommand<'args> for Shorten<'args> {
    const LONG_OPTIONS: &'static [WOption<'static>] = &[
        // Support both spellings: docs use --char, older fish accepted --chars.
        wopt(L!("char"), RequiredArgument, 'c'),
        wopt(L!("chars"), RequiredArgument, 'c'),
        wopt(L!("max"), RequiredArgument, 'm'),
        wopt(L!("no-newline"), NoArgument, 'N'),
        wopt(L!("left"), NoArgument, 'l'),
        wopt(L!("quiet"), NoArgument, 'q'),
    ];
    const SHORT_OPTIONS: &'static wstr = L!("c:m:Nlq");

    fn parse_opt(&mut self, c: char, arg: Option<&'args wstr>) -> Result<(), StringError<'_>> {
        match c {
            'c' => {
                self.ellipsis = arg.unwrap();
                self.ellipsis_width = visible_width(self.ellipsis);
            }
            'm' => {
                let arg = arg.unwrap();
                self.max = Some(
                    Self::parse_arg_number(arg)?
                        .try_into()
                        .map_err(|_| err_fmt!(Error::INVALID_MAX_VALUE, arg))?,
                );
            }
            'N' => self.no_newline = true,
            'l' => self.shorten_from = Direction::Left,
            'q' => self.quiet = true,
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
        let mut min_width = usize::MAX;
        let mut inputs = Vec::new();

        let iter = arguments(args, optind, streams);

        if self.max == Some(0) {
            // Special case: Max of 0 means no shortening.
            // This makes this more reusable, so you don't need special-cases like
            //
            // if test $shorten -gt 0
            //     string shorten -m $shorten whatever
            // else
            //     echo whatever
            // end
            for InputValue { arg, .. } in iter {
                streams.out.appendln(&arg);
            }
            return Ok(());
        }

        for InputValue { arg, .. } in iter {
            // Visible width only makes sense line-wise.
            // So either we have no-newlines (which means we shorten on the first newline),
            // or we handle the lines separately.
            let mut splits = arg.split('\n').peekable();
            if self.no_newline && splits.peek().is_some() {
                let mut s = match self.shorten_from {
                    Direction::Right => splits.next(),
                    Direction::Left => splits.last(),
                }
                .unwrap()
                .to_owned();
                s.push_utfstr(&self.ellipsis);
                let width = visible_width(&s);

                if width > 0 && width < min_width {
                    min_width = width;
                }
                inputs.push(s);
            } else {
                for s in splits {
                    let width = visible_width(s);
                    if width > 0 && width < min_width {
                        min_width = width;
                    }
                    inputs.push(s.into());
                }
            }
        }

        let ourmax: usize = self.max.unwrap_or(min_width);

        let (ell, ell_width) = if self.ellipsis_width > ourmax {
            // If we can't even print our ellipsis, we substitute nothing,
            // truncating instead.
            (L!(""), 0)
        } else {
            (self.ellipsis, self.ellipsis_width)
        };

        let mut nsub = 0usize;
        // We could also error out here if the width of our ellipsis is larger
        // than the target width.
        // That seems excessive - specifically because the ellipsis on LANG=C
        // is "..." (width 3!).

        let widths = crate::string_display::WidthSettings::default();
        for line in inputs {
            let left = self.shorten_from == Direction::Left;
            let range = widths.shorten_range(&line, ourmax, ell_width, left);
            let changed = range != (0..line.len());
            if self.quiet {
                if changed {
                    return Ok(());
                }
                continue;
            }
            nsub += usize::from(changed);
            if changed && left {
                streams.out.append(ell);
            }
            streams.out.append(&line[range]);
            if changed && !left {
                streams.out.append(ell);
            }
            streams.out.appendln(L!(""));
        }

        // Return true if we have shortened something and false otherwise.
        if nsub > 0 {
            Ok(())
        } else {
            Err(STATUS_CMD_ERROR)
        }
    }
}
