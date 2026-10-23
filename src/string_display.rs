//! Portable terminal-width measurement and string display transforms.
use fish_widestring::prelude::*;
use std::ops::Range;
use std::sync::atomic::Ordering;

#[derive(Clone, Copy)]
pub(crate) struct WidthSettings {
    pub ambiguous: usize,
    pub emoji: usize,
}

impl Default for WidthSettings {
    fn default() -> Self {
        Self {
            ambiguous: fish_fallback::FISH_AMBIGUOUS_WIDTH.load(Ordering::Relaxed),
            emoji: fish_fallback::FISH_EMOJI_WIDTH.load(Ordering::Relaxed),
        }
    }
}

impl WidthSettings {
    #[cfg(windows)]
    pub(crate) fn from_overrides(ambiguous: Option<&str>, emoji: Option<&str>) -> Self {
        Self {
            ambiguous: ambiguous
                .and_then(|value| value.trim().parse::<i32>().ok())
                .unwrap_or(1)
                .max(0) as usize,
            emoji: emoji.map_or(2, |value| {
                value.trim().parse::<i32>().unwrap_or(1).clamp(1, 2)
            }) as usize,
        }
    }

    pub(crate) fn character_width(self, c: char) -> Option<usize> {
        fish_fallback::fish_wcwidth_with_options(c, self.ambiguous, self.emoji)
    }

    fn visible_character_width(self, c: char) -> isize {
        if c == '\x08' {
            -1
        } else {
            self.character_width(c).unwrap_or_default() as isize
        }
    }
}

/// Iterate visible characters and whole runs of terminal escapes. Remember the
/// last terminators so repeated unfinished OSC/title escapes take linear time.
struct Tokens<'a> {
    input: &'a wstr,
    pos: usize,
    last_osc_end: usize,
    last_title_end: usize,
    widths: WidthSettings,
}

impl<'a> Tokens<'a> {
    fn new(input: &'a wstr, widths: WidthSettings) -> Self {
        let mut result = Self {
            input,
            pos: 0,
            last_osc_end: 0,
            last_title_end: 0,
            widths,
        };
        for (pos, c) in input.chars().enumerate() {
            if c == '\x07' {
                result.last_osc_end = pos;
            } else if c == '\\' && pos > 0 && input.char_at(pos - 1) == '\x1b' {
                result.last_osc_end = pos;
                result.last_title_end = pos;
            }
        }
        result
    }

    fn escape_length(&self) -> Option<usize> {
        let input = self.input.slice_from(self.pos);
        if input.char_at(0) != '\x1b' {
            return None;
        }
        match input.char_at(1) {
            ']' if self.last_osc_end < self.pos + 2 => Some(2),
            'k' if self.last_title_end < self.pos + 2 => Some(2),
            _ => escape_code_length(input),
        }
    }
}

impl Iterator for Tokens<'_> {
    type Item = (Range<usize>, isize);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.input.len() {
            return None;
        }
        let start = self.pos;
        if let Some(length) = self.escape_length() {
            self.pos += length;
            while self.pos < self.input.len() {
                let Some(length) = self.escape_length() else {
                    break;
                };
                self.pos += length;
            }
            Some((start..self.pos, 0))
        } else {
            self.pos += 1;
            Some((
                start..self.pos,
                self.widths
                    .visible_character_width(self.input.char_at(start)),
            ))
        }
    }
}

/// Terminal columns, ignoring recognized escapes and clipping backspaces at zero.
#[cfg(not(windows))]
pub(crate) fn visible_width(input: &wstr) -> usize {
    WidthSettings::default().visible_width(input)
}

impl WidthSettings {
    pub(crate) fn visible_width(self, input: &wstr) -> usize {
        Tokens::new(input, self).fold(0usize, |width, (_, next)| width.saturating_add_signed(next))
    }

    /// One visible width per line. Carriage returns reset the cursor within a line.
    pub(crate) fn line_widths(self, input: &wstr) -> impl Iterator<Item = usize> + '_ {
        input.split('\n').map(move |line| {
            line.split('\r')
                .map(|part| self.visible_width(part))
                .max()
                .unwrap_or(0)
        })
    }
}

pub(crate) fn pad(
    input: &wstr,
    input_width: usize,
    target_width: usize,
    character: char,
    character_width: usize,
    right: bool,
    center: bool,
) -> WString {
    let total = target_width.saturating_sub(input_width);
    let (left, right) = match (right, center) {
        (false, false) => (total, 0),
        (true, false) => (0, total),
        (false, true) => (total - total / 2, total / 2),
        (true, true) => (total / 2, total - total / 2),
    };
    let chars = |width| std::iter::repeat_n(character, width / character_width);
    let spaces = |width| std::iter::repeat_n(' ', width % character_width);
    chars(left)
        .chain(spaces(left))
        .chain(input.chars())
        .chain(spaces(right))
        .chain(chars(right))
        .collect()
}

/// The retained codepoint range. Escape runs are never split. An unchanged
/// string returns its full range; the caller places the ellipsis on the cut side.
impl WidthSettings {
    pub(crate) fn shorten_range(
        self,
        input: &wstr,
        max: usize,
        ellipsis_width: usize,
        left: bool,
    ) -> Range<usize> {
        if self.visible_width(input) <= max {
            return 0..input.len();
        }
        let available = max.saturating_sub(ellipsis_width);
        if !left {
            let mut width = 0usize;
            for (range, next) in Tokens::new(input, self) {
                if next > 0 && width.saturating_add(next as usize) > available {
                    return 0..range.start;
                }
                width = width.saturating_add_signed(next);
            }
            return 0..input.len();
        }

        // A suffix's clipped width is its signed sum minus its lowest prefix sum.
        // Calculate those from right to left instead of rescanning every suffix.
        // The sentinel marks interior escape characters, where cutting is forbidden.
        let mut weights = vec![isize::MIN; input.len()];
        for (range, width) in Tokens::new(input, self) {
            weights[range.start] = width;
        }
        let (mut sum, mut minimum) = (0isize, 0isize);
        let mut start = input.len();
        for (pos, weight) in weights.into_iter().enumerate().rev() {
            if weight == isize::MIN {
                continue;
            }
            sum += weight;
            minimum = 0.min(minimum + weight);
            if (sum - minimum) as usize <= available {
                start = pos;
            }
        }
        start..input.len()
    }
}

/// Returns the number of characters in the escape code starting at 'code'. We only handle sequences
/// that begin with \x1B. If it doesn't we return zero. We also return zero if we don't recognize
/// the escape sequence.
pub fn escape_code_length(code: &wstr) -> Option<usize> {
    if code.char_at(0) != '\x1B' {
        return None;
    }

    is_screen_name_escape_seq(code)
        .or_else(|| is_osc_escape_seq(code))
        .or_else(|| is_three_byte_escape_seq(code))
        .or_else(|| is_csi_style_escape_seq(code))
        .or_else(|| is_two_byte_escape_seq(code))
}

/// Does this look like the escape sequence for setting a screen name?
fn is_screen_name_escape_seq(code: &wstr) -> Option<usize> {
    // Tmux escapes start with `\ePtmux;` and end also in `\e\\`,
    // so we can just handle them here.
    let tmux_seq = L!("Ptmux;");
    let mut is_tmux = false;
    if code.char_at(1) != 'k' {
        if code.starts_with(tmux_seq) {
            is_tmux = true;
        } else {
            return None;
        }
    }
    let screen_name_end_sentinel = L!("\x1B\\");
    let mut offset = 2;
    let escape_sequence_end;
    loop {
        let Some(pos) = code[offset..].find(screen_name_end_sentinel) else {
            // Consider just <esc>k to be the code.
            // (note: for the tmux sequence this is broken, but since we have no idea...)
            escape_sequence_end = 2;
            break;
        };
        let screen_name_end = offset + pos;
        // The tmux sequence requires that all escapes in the payload sequence
        // be doubled. So if we have \e\e\\ that's still not the end.
        if is_tmux {
            let mut esc_count = 0;
            let mut i = screen_name_end;
            while i > 0 && code.as_char_slice()[i - 1] == '\x1B' {
                i -= 1;
                if i > 0 {
                    esc_count += 1;
                }
            }
            if esc_count % 2 == 1 {
                offset = screen_name_end + 1;
                continue;
            }
        }
        escape_sequence_end = screen_name_end + screen_name_end_sentinel.len();
        break;
    }
    Some(escape_sequence_end)
}

/// Operating System Command (OSC) escape codes, used by iTerm2 and others:
/// ESC followed by ], terminated by either BEL or escape + backslash.
/// See <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html>
/// and <https://iterm2.com/documentation-escape-codes.html>.
fn is_osc_escape_seq(code: &wstr) -> Option<usize> {
    if code.char_at(1) == ']' {
        // Start at 2 to skip over <esc>].
        let mut cursor = 2;
        while cursor < code.len() {
            let code = code.as_char_slice();
            // Consume a sequence of characters up to <esc>\ or <bel>.
            if code[cursor] == '\x07' || (code[cursor] == '\\' && code[cursor - 1] == '\x1B') {
                return Some(cursor + 1);
            }
            cursor += 1;
        }
    }
    None
}

/// Generic VT100 three byte sequence: `CSI` followed by something in the range @ through _.
fn is_three_byte_escape_seq(code: &wstr) -> Option<usize> {
    if code.char_at(1) == '[' && (code.char_at(2) >= '@' && code.char_at(2) <= '_') {
        return Some(3);
    }
    None
}

/// Generic VT100 two byte sequence: `<esc>` followed by something in the range @ through _.
fn is_two_byte_escape_seq(code: &wstr) -> Option<usize> {
    if code.char_at(1) >= '@' && code.char_at(1) <= '_' {
        return Some(2);
    }
    None
}

/// Generic VT100 CSI-style sequence. `<esc>`, followed by zero or more ASCII characters NOT in
/// the range `[@,_],` followed by one character in that range.
/// This will also catch color sequences.
fn is_csi_style_escape_seq(code: &wstr) -> Option<usize> {
    if code.char_at(1) != '[' {
        return None;
    }

    // Start at 2 to skip over <esc>[
    let mut cursor = 2;
    while cursor < code.len() {
        // Consume a sequence of ASCII characters not in the range [@, ~].
        let widechar = code.as_char_slice()[cursor];

        // If we're not in ASCII, just stop.
        if !widechar.is_ascii() {
            break;
        }

        // If we're the end character, then consume it and then stop.
        if ('@'..'~').contains(&widechar) {
            cursor += 1;
            break;
        }
        cursor += 1;
    }
    // cursor now indexes just beyond the end of the sequence (or at the terminating zero).
    Some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTHS: WidthSettings = WidthSettings {
        ambiguous: 1,
        emoji: 2,
    };

    #[test]
    fn ansi_backspaces_clip_only_visible_columns() {
        assert_eq!(WIDTHS.visible_width(L!("\x1b[31m\x08\x08\x08\x08")), 0);
        assert_eq!(WIDTHS.visible_width(L!("\x1b[31mx\x08\x08y")), 1);
        assert_eq!(WIDTHS.visible_width(L!("\x1b]title\x08\x07x")), 1);
    }

    #[test]
    fn left_shortening_matches_suffix_measurement_with_control_characters() {
        // Exhaust small mixtures of positive, zero and negative width characters.
        // The reference deliberately measures every possible suffix independently.
        let alphabet = ['a', '文', '\x08', '\x07', '\u{0301}'];
        for seed in 0..alphabet.len().pow(5) {
            let mut encoded = seed;
            let mut input = WString::new();
            for _ in 0..5 {
                input.push(alphabet[encoded % alphabet.len()]);
                encoded /= alphabet.len();
            }
            for max in 1..=4 {
                let expected = if WIDTHS.visible_width(&input) <= max {
                    0
                } else {
                    (0..input.len())
                        .find(|&pos| WIDTHS.visible_width(input.slice_from(pos)) < max)
                        .unwrap_or(input.len())
                };
                assert_eq!(
                    WIDTHS.shorten_range(&input, max, 1, true).start,
                    expected,
                    "{input:?}, max={max}"
                );
            }
        }
    }

    #[test]
    fn escape_runs_remain_whole_on_both_cut_sides() {
        let input = L!("\x1b[31ms\x1b[32mt\x1b[33mrin\x1b[34m\x1b[35mg-shorten");
        let left = WIDTHS.shorten_range(input, 12, 1, true);
        assert_eq!(&input[left], L!("in\x1b[34m\x1b[35mg-shorten"));
        let right = WIDTHS.shorten_range(input, 6, 1, false);
        assert_eq!(
            &input[right],
            L!("\x1b[31ms\x1b[32mt\x1b[33mrin\x1b[34m\x1b[35m")
        );
    }

    #[test]
    fn large_suffixes_and_unterminated_escapes_are_bounded_work() {
        let input = WString::from("a".repeat(200_000));
        assert_eq!(WIDTHS.shorten_range(&input, 3, 1, true), 199_998..200_000);
        let input = WString::from("\x1b]x\x1bkx".repeat(100_000));
        assert_eq!(WIDTHS.visible_width(&input), 200_000);
    }
}
