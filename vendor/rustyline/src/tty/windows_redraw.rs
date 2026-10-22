//! Bounded, independently paintable physical rows for the Windows console.
//!
//! The ordinary reader has already wrapped these strings. Only monotonic text,
//! SGR, and erase-to-end controls are safe to diff: cursor-moving or otherwise
//! unknown controls retain the console renderer's conservative full redraw.

use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_ROWS: usize = 4096;
const MAX_STYLE_BYTES: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Row {
    /// Replay from the default SGR state, including styles spanning newlines.
    style: String,
    text: String,
    pub(super) columns: usize,
}

impl Row {
    pub(super) fn append_to(&self, output: &mut String, ansi: bool) {
        if ansi {
            output.push_str("\x1b[0m");
            output.push_str(&self.style);
        }
        output.push_str(&self.text);
    }
}

#[derive(Debug)]
pub(super) struct Frame {
    pub(super) rows: Vec<Row>,
    pub(super) erase_down: bool,
    final_style: String,
    ansi: bool,
}

impl Frame {
    pub(super) fn parse(source: &str, columns: usize, ansi: bool) -> Option<Self> {
        if source.len() > MAX_FRAME_BYTES || columns == 0 {
            return None;
        }
        let mut rows = Vec::new();
        let mut erase_down = false;
        let mut style = String::new();
        let mut row = Row {
            style: String::new(),
            text: String::new(),
            columns: 0,
        };
        let mut remaining = source;
        let mut stored_bytes = source.len();
        while !remaining.is_empty() {
            if let Some(parameters) = remaining.strip_prefix("\x1b[") {
                if !ansi {
                    return None;
                }
                let end = parameters
                    .bytes()
                    .position(|byte| !(byte.is_ascii_digit() || matches!(byte, b';' | b':')))?;
                let final_byte = parameters.as_bytes()[end];
                if !matches!(final_byte, b'm' | b'J' | b'K') {
                    return None; // Do not slice through a malformed UTF-8 final character.
                }
                let sequence = &remaining[..end + 3];
                let parameters = &parameters[..end];
                match final_byte {
                    b'm' => {
                        // Only a leading zero is unambiguously a full reset;
                        // zeros later in an RGB sequence are color channels.
                        let first = parameters.split(';').next().unwrap_or("");
                        if first.is_empty() || first.parse::<u32>() == Ok(0) {
                            style.clear();
                        }
                        if !(parameters.is_empty() || parameters == "0") {
                            style.push_str(sequence);
                        }
                        if style.len() > MAX_STYLE_BYTES {
                            return None;
                        }
                        row.text.push_str(sequence);
                    }
                    // Starship starts its prompt with CSI J. Replaying that
                    // clear would erase every cached row before painting. Our
                    // bounded stale-tail cleanup supplies the same effect for
                    // the area owned by this editor, after the text is written.
                    b'J' if (parameters.is_empty() || parameters == "0") && style.is_empty() => {
                        erase_down = true;
                    }
                    b'K' if (parameters.is_empty() || parameters == "0") && style.is_empty() => {}
                    _ => return None,
                }
                remaining = &remaining[sequence.len()..];
            } else if remaining.starts_with('\n') || remaining.starts_with("\r\n") {
                remaining = if let Some(rest) = remaining.strip_prefix("\r\n") {
                    rest
                } else {
                    &remaining[1..]
                };
                stored_bytes = stored_bytes.checked_add(style.len())?;
                if rows.len() + 1 >= MAX_ROWS || stored_bytes > MAX_FRAME_BYTES {
                    return None;
                }
                rows.push(row);
                row = Row {
                    style: style.clone(),
                    text: String::new(),
                    columns: 0,
                };
            } else {
                let end = remaining.find(char::is_control).unwrap_or(remaining.len());
                if end == 0 {
                    return None;
                }
                let text = &remaining[..end];
                for grapheme in text.graphemes(true) {
                    row.columns = row.columns.checked_add(grapheme.width())?;
                    if row.columns > columns {
                        return None; // The supplied physical rows did not match layout.
                    }
                }
                row.text.push_str(text);
                remaining = &remaining[end..];
            }
        }
        rows.push(row);
        Some(Self {
            rows,
            erase_down,
            final_style: style,
            ansi,
        })
    }

    pub(super) fn matches_end(&self, row: usize, column: usize) -> bool {
        self.rows.len() == row + 1 && self.rows.last().is_some_and(|last| last.columns == column)
    }

    pub(super) fn append_row(&self, index: usize, output: &mut String) {
        self.rows[index].append_to(output, self.ansi);
    }

    pub(super) fn append_all(&self, output: &mut String) {
        for (index, row) in self.rows.iter().enumerate() {
            if index != 0 {
                output.push('\n');
            }
            row.append_to(output, self.ansi);
        }
    }

    pub(super) fn append_final_style(&self, output: &mut String) {
        if self.ansi {
            output.push_str("\x1b[0m");
            output.push_str(&self.final_style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(source: &str) -> Frame {
        Frame::parse(source, 80, true).unwrap()
    }

    #[test]
    fn menu_selection_changes_only_the_two_selected_rows() {
        let before = frame(
            "\x1b[Jheader\n> \n\x1b[36mSearch:\x1b[0m\n\x1b[7m> alpha\x1b[0m\n  beta\n  gamma",
        );
        let after = frame(
            "\x1b[Jheader\n> \n\x1b[36mSearch:\x1b[0m\n  alpha\n\x1b[7m> beta\x1b[0m\n  gamma",
        );
        let changed: Vec<_> = before
            .rows
            .iter()
            .zip(&after.rows)
            .enumerate()
            .filter_map(|(index, (old, new))| (old != new).then_some(index))
            .collect();
        assert_eq!(changed, [3, 4]);
    }

    #[test]
    fn inherited_truecolor_and_resets_survive_independent_row_painting() {
        let styled = frame("\x1b[38;2;0;20;30mfirst\nsecond\x1b[0m\nplain");
        let mut output = String::new();
        styled.append_row(1, &mut output);
        assert_eq!(output, "\x1b[0m\x1b[38;2;0;20;30msecond\x1b[0m");
        output.clear();
        styled.append_row(2, &mut output);
        assert_eq!(output, "\x1b[0mplain");
        output.clear();
        styled.append_final_style(&mut output);
        assert_eq!(output, "\x1b[0m");
        assert_ne!(styled.rows[1], frame("second\x1b[0m").rows[0]);
    }

    #[test]
    fn end_erases_are_replaced_by_bounded_cleanup() {
        let frame = frame("\x1b[J\nheader\x1b[0K\n> draft\x1b[0J");
        let mut output = String::new();
        frame.append_all(&mut output);
        assert!(!output.contains('J'));
        assert!(!output.contains('K'));
        assert!(frame.erase_down);
        assert!(frame.matches_end(2, 7));
    }

    #[test]
    fn physical_rows_keep_wide_graphemes_and_exact_width_boundaries() {
        let frame = Frame::parse("a🐟e\u{301}\n", 4, true).unwrap();
        assert_eq!(frame.rows[0].columns, 4);
        assert!(frame.matches_end(1, 0));
        assert!(Frame::parse("a🐟e\u{301}x", 4, true).is_none());
        let frame = Frame::parse("first\r\nnext", 8, false).unwrap();
        assert!(frame.matches_end(1, 4));
        let mut output = String::new();
        frame.append_all(&mut output);
        assert_eq!(output, "first\nnext");
    }

    #[test]
    fn unsupported_or_unbounded_frames_require_full_redraw() {
        for source in [
            "\x1b[2Jtext",
            "\x1b[Htext",
            "\x1b]8;;url\x07link",
            "a\rb",
            "a\tb",
            "\x1b[31",
            "\x1b[31🐟",
            "\x1b[41mred\x1b[K\x1b[0m\n> ",
            "\x1b[41mred\x1b[J\x1b[0m\n> ",
        ] {
            assert!(Frame::parse(source, 80, true).is_none(), "{source:?}");
        }
        assert!(Frame::parse("\x1b[31mtext", 80, false).is_none());
        assert!(
            Frame::parse(&"x".repeat(MAX_FRAME_BYTES + 1), MAX_FRAME_BYTES + 1, true).is_none()
        );
        assert!(Frame::parse(&"\n".repeat(MAX_ROWS), 80, true).is_none());
        assert!(Frame::parse(&"\x1b[31m".repeat(MAX_STYLE_BYTES), 80, true).is_none());
    }
}
