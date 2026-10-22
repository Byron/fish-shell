//! Shared Fish script, URL, variable-name and regular-expression escaping.

use fish_feature_flags::{FeatureFlag, feature_test};
use fish_widestring::{
    ANY_CHAR, ANY_STRING, ANY_STRING_RECURSIVE, ASCII_MAX, BRACE_BEGIN, BRACE_END, BRACE_SEP,
    BRACE_SPACE, BYTE_MAX, HOME_DIRECTORY, INTERNAL_SEPARATOR, L, PROCESS_EXPAND_SELF,
    PROCESS_EXPAND_SELF_STR, SLICE_BEGIN, SLICE_END, UCS2_MAX, VARIABLE_EXPAND,
    VARIABLE_EXPAND_SINGLE, WExt as _, WString, bytes2wcstring, decode_byte_from_char,
    fish_reserved_codepoint, wcs2bytes, wstr,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EscapeStringStyle {
    Script(EscapeFlags),
    Url,
    Var,
    Regex,
}

impl Default for EscapeStringStyle {
    fn default() -> Self {
        Self::Script(EscapeFlags::default())
    }
}

impl TryFrom<&wstr> for EscapeStringStyle {
    type Error = &'static wstr;
    fn try_from(s: &wstr) -> Result<Self, Self::Error> {
        use EscapeStringStyle::*;
        match s {
            s if s == "script" => Ok(Self::default()),
            s if s == "var" => Ok(Var),
            s if s == "url" => Ok(Url),
            s if s == "regex" => Ok(Regex),
            _ => Err(L!("Invalid escape style")),
        }
    }
}

/// Options for the [`escape_string()`] function. These are only applicable when the escape style is
/// [`EscapeStringStyle::Script`].
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct EscapeFlags {
    /// Do not escape special fish syntax characters like the semicolon. Only escape non-printable
    /// characters and backslashes.
    pub no_printables: bool,
    /// Do not try to use 'simplified' quoted escapes, and do not use empty quotes as the empty
    /// string.
    pub no_quoted: bool,
    /// Do not escape tildes.
    pub no_tilde: bool,
    /// Replace non-printable control characters with Unicode symbols.
    pub symbolic: bool,
    /// Escape commas.
    pub comma: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnescapeStringStyle {
    Script(UnescapeFlags),
    Url,
    Var,
}

impl Default for UnescapeStringStyle {
    fn default() -> Self {
        Self::Script(UnescapeFlags::default())
    }
}

impl TryFrom<&wstr> for UnescapeStringStyle {
    type Error = &'static wstr;
    fn try_from(s: &wstr) -> Result<Self, Self::Error> {
        use UnescapeStringStyle::*;
        match s {
            s if s == "script" => Ok(Self::default()),
            s if s == "var" => Ok(Var),
            s if s == "url" => Ok(Url),
            _ => Err(L!("Invalid escape style")),
        }
    }
}

/// Options for unescape_string functions.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct UnescapeFlags {
    /// Escape special fish syntax characters like the semicolon.
    pub special: bool,
    /// Allow incomplete escape sequences.
    pub incomplete: bool,
    /// Don't handle backslash escapes.
    pub no_backslashes: bool,
}
/// Replace special characters with backslash escape sequences. Newline is replaced with `\n`, etc.
pub fn escape(s: &wstr) -> WString {
    escape_string(s, EscapeStringStyle::Script(EscapeFlags::default()))
}

/// Replace special characters with backslash escape sequences. Newline is replaced with `\n`, etc.
pub fn escape_string(s: &wstr, style: EscapeStringStyle) -> WString {
    match style {
        EscapeStringStyle::Script(flags) => escape_string_script(s, flags),
        EscapeStringStyle::Url => escape_string_url(s),
        EscapeStringStyle::Var => escape_string_var(s),
        EscapeStringStyle::Regex => escape_string_pcre2(s),
    }
}

/// Escape a string in a fashion suitable for using in fish script.
fn escape_string_script(input: &wstr, flags: EscapeFlags) -> WString {
    let escape_printables = !flags.no_printables;
    let escape_comma = flags.comma;
    let no_quoted = flags.no_quoted;
    let no_tilde = flags.no_tilde;
    let no_qmark = feature_test(FeatureFlag::QuestionMarkNoGlob);
    let symbolic = flags.symbolic;

    assert!(
        !symbolic || !escape_printables,
        "symbolic implies escape-no-printables"
    );

    let mut need_escape = false;
    let mut need_complex_escape = no_tilde && input.char_at(0) == '~';
    let mut double_quotes = 0;
    let mut single_quotes = 0;
    let mut dollars = 0;

    if !no_quoted && input.is_empty() {
        return L!("''").to_owned();
    }

    let mut out = WString::with_capacity(input.len());

    for (i, c) in input.chars().enumerate() {
        if let Some(val) = decode_byte_from_char(c) {
            out += "\\X";

            let nibble1 = val / 16;
            let nibble2 = val % 16;

            out.push(char::from_digit(nibble1.into(), 16).unwrap());
            out.push(char::from_digit(nibble2.into(), 16).unwrap());
            need_escape = true;
            need_complex_escape = true;
            continue;
        }
        match c {
            '\t' => {
                if symbolic {
                    out.push('␉');
                } else {
                    out += L!("\\t");
                }
                need_escape = true;
                need_complex_escape = true;
            }
            '\n' => {
                if symbolic {
                    out.push('␤');
                } else {
                    out += L!("\\n");
                }
                need_escape = true;
                need_complex_escape = true;
            }
            '\x08' => {
                if symbolic {
                    out.push('␈');
                } else {
                    out += L!("\\b");
                }
                need_escape = true;
                need_complex_escape = true;
            }
            '\r' => {
                if symbolic {
                    out.push('␍');
                } else {
                    out += L!("\\r");
                }
                need_escape = true;
                need_complex_escape = true;
            }
            '\x1B' => {
                if symbolic {
                    out.push('␛');
                } else {
                    out += L!("\\e");
                }
                need_escape = true;
                need_complex_escape = true;
            }
            '\x7F' => {
                if symbolic {
                    out.push('␡');
                } else {
                    out += L!("\\x7f");
                }
                need_escape = true;
                need_complex_escape = true;
            }
            '\\' | '\'' => {
                need_escape = true;
                if c == '\'' {
                    single_quotes += 1;
                }
                if escape_printables || (c == '\\' && !symbolic) {
                    out.push('\\');
                }
                out.push(c);
            }
            ANY_CHAR => {
                // See #1614
                out.push('?');
            }
            ANY_STRING => {
                out.push('*');
            }
            ANY_STRING_RECURSIVE => {
                out += L!("**");
            }
            ',' => {
                if escape_comma {
                    need_escape = true;
                    out.push('\\');
                }
                out.push(c);
            }

            '&' | '$' | ' ' | '#' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '?' | '*'
            | '|' | ';' | '"' | '%' | '~' => {
                if c == '"' {
                    double_quotes += 1;
                }
                if c == '$' {
                    dollars += 1;
                }
                let char_is_normal = (c == '~' && no_tilde)
                    || (c == '?' && no_qmark)
                    || (c == '#'
                        && i.checked_sub(1)
                            .map(|previ| input.char_at(previ).is_alphanumeric())
                            .unwrap_or_default());
                if !char_is_normal {
                    need_escape = true;
                    if escape_printables {
                        out.push('\\');
                    }
                }
                out.push(c);
            }
            '\x00'..='\x19' => {
                let cval = u32::from(c);
                need_escape = true;
                need_complex_escape = true;

                if symbolic {
                    out.push(char::from_u32(0x2400 + cval).unwrap());
                    continue;
                }

                if cval < 27 && cval != 0 {
                    out.push('\\');
                    out.push('c');
                    out.push(char::from_u32(u32::from(b'a') + cval - 1).unwrap());
                    continue;
                }

                let nibble = cval % 16;
                out.push('\\');
                out.push('x');
                out.push(if cval > 15 { '1' } else { '0' });
                out.push(char::from_digit(nibble, 16).unwrap());
            }
            _ => out.push(c),
        }
    }

    // Use quoted escaping if possible, since most people find it easier to read.
    if !no_quoted && need_escape && !need_complex_escape && escape_printables {
        let quote = if single_quotes > double_quotes + dollars {
            '"'
        } else {
            '\''
        };
        out.clear();
        out.reserve(2 + input.len());
        out.push(quote);
        out.push_utfstr(&escape_string_with_quote(
            input,
            Some(quote),
            EscapeFlags::default(),
        ));
        out.push(quote);
    }

    out
}

/// Attempts to escape the string 'cmd' using the given quote type, as determined by the quote
/// character. The quote can be a single quote or double quote, or L'\0' to indicate no quoting (and
/// thus escaping should be with backslashes). Optionally do not escape tildes.
pub fn escape_string_with_quote(
    cmd: &wstr,
    quote: Option<char>,
    escape_flags: EscapeFlags,
) -> WString {
    let Some(quote) = quote else {
        return escape_string(cmd, EscapeStringStyle::Script(escape_flags));
    };
    // Here we are going to escape a string with quotes.
    // A few characters cannot be represented inside quotes, e.g. newlines. In that case,
    // terminate the quote and then re-enter it.
    let mut result = WString::new();
    result.reserve(cmd.len());
    for c in cmd.chars() {
        match c {
            '\n' => {
                for c in [quote, '\\', 'n', quote] {
                    result.push(c);
                }
            }
            '\t' => {
                for c in [quote, '\\', 't', quote] {
                    result.push(c);
                }
            }
            '\x08' => {
                for c in [quote, '\\', 'b', quote] {
                    result.push(c);
                }
            }
            '\r' => {
                for c in [quote, '\\', 'r', quote] {
                    result.push(c);
                }
            }
            '\\' => {
                result.push_str("\\\\");
            }
            '$' => {
                if quote == '"' {
                    result.push('\\');
                }
                result.push('$');
            }
            _ => {
                if c == quote {
                    result.push('\\');
                }
                result.push(c);
            }
        }
    }
    result
}

/// Test whether the char is a valid hex digit as used by the `escape_string_*()` functions.
/// Note this only considers uppercase characters.
fn is_upper_hex_digit(c: char) -> bool {
    matches!(c, '0'..='9' | 'A'..='F')
}

/// Return the high and low nibbles of a byte, as uppercase hex characters.
fn byte_to_hex(byte: u8) -> (char, char) {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let high = byte >> 4;
    let low = byte & 0xF;
    (HEX[high as usize].into(), HEX[low as usize].into())
}

/// Escape a string in a fashion suitable for using as a URL. Store the result in out_str.
fn escape_string_url(input: &wstr) -> WString {
    let narrow = wcs2bytes(input);
    let mut out = WString::new();
    for byte in narrow {
        if (byte & 0x80) == 0 {
            let c = char::from_u32(u32::from(byte)).unwrap();
            if c.is_alphanumeric() || b"/.~-_".contains(&byte) {
                // The above characters don't need to be encoded.
                out.push(c);
                continue;
            }
        }
        // All other chars need to have their narrow representation encoded in hex.
        let (high, low) = byte_to_hex(byte);
        out.push('%');
        out.push(high);
        out.push(low);
    }
    out
}

/// Escape a string in a fashion suitable for using as a fish var name. Store the result in out_str.
fn escape_string_var(input: &wstr) -> WString {
    let mut prev_was_hex_encoded = false;
    let narrow = wcs2bytes(input);
    let mut out = WString::new();
    for c in narrow {
        let ch: char = c.into();
        if ((c & 0x80) == 0 && ch.is_alphanumeric())
            && (!prev_was_hex_encoded || !is_upper_hex_digit(ch))
        {
            // ASCII alphanumerics don't need to be encoded.
            if prev_was_hex_encoded {
                out.push('_');
                prev_was_hex_encoded = false;
            }
            out.push(ch);
        } else if c == b'_' {
            // Underscores are encoded by doubling them.
            out.push_str("__");
            prev_was_hex_encoded = false;
        } else {
            // All other chars need to have their narrow representation encoded in hex.
            let (high, low) = byte_to_hex(c);
            out.push('_');
            out.push(high);
            out.push(low);
            prev_was_hex_encoded = true;
        }
    }
    if prev_was_hex_encoded {
        out.push('_');
    }
    out
}

/// Escapes a string for use in a regex string. Not safe for use with `eval` as only
/// characters reserved by PCRE2 are escaped.
/// \param in is the raw string to be searched for literally when substituted in a PCRE2 expression.
fn escape_string_pcre2(input: &wstr) -> WString {
    let mut out = WString::new();
    out.reserve(input.len() + input.len() / 2);

    for c in input.chars() {
        if c == '\n' {
            out.push_str("\\n");
            continue;
        }
        if [
            '.', '^', '$', '*', '+', '(', ')', '?', '[', '{', '}', '\\', '|',
            // these two only *need* to be escaped within a character class, and technically it
            // makes no sense to ever use process substitution output to compose a character class,
            // but...
            '-', ']',
        ]
        .contains(&c)
        {
            out.push('\\');
        }
        out.push(c);
    }

    out
}

/// Escape a string so that it may be inserted into a double-quoted string.
/// This permits ownership transfer.
pub fn escape_string_for_double_quotes(input: &wstr) -> WString {
    // We need to escape backslashes, double quotes, and dollars only.
    let mut result = input.to_owned();
    let mut idx = result.len();
    while idx > 0 {
        idx -= 1;
        if ['\\', '$', '"'].contains(&result.char_at(idx)) {
            result.insert(idx, '\\');
        }
    }
    result
}

pub fn unescape_string(input: &wstr, style: UnescapeStringStyle) -> Option<WString> {
    match style {
        UnescapeStringStyle::Script(flags) => unescape_string_internal(input, flags),
        UnescapeStringStyle::Url => unescape_string_url(input),
        UnescapeStringStyle::Var => unescape_string_var(input),
    }
}

/// Length of a backslash followed by an LF or CRLF line ending, or zero.
/// Call only where backslashes can continue a line, never inside single quotes.
pub fn line_continuation_length(input: &[char]) -> usize {
    match input {
        ['\\', '\n', ..] => 2,
        ['\\', '\r', '\n', ..] => 3,
        _ => 0,
    }
}

/// Returns the unescaped version of input, or None on error.
fn unescape_string_internal(input: &wstr, flags: UnescapeFlags) -> Option<WString> {
    let mut result = WString::new();
    result.reserve(input.len());

    let unescape_special = flags.special;
    let allow_incomplete = flags.incomplete;
    let ignore_backslashes = flags.no_backslashes;
    let allow_percent_self = !feature_test(FeatureFlag::RemovePercentSelf);

    // The positions of open braces.
    let mut braces = vec![];
    // The positions of variable expansions or brace ","s.
    // We only read braces as expanders if there's a variable expansion or "," in them.
    let mut vars_or_seps = vec![];
    let mut brace_count = 0;
    let mut potential_word_start = None;

    let mut errored = false;
    #[derive(PartialEq)]
    enum Mode {
        Unquoted,
        SingleQuotes,
        DoubleQuotes,
    }
    let mut mode = Mode::Unquoted;

    let mut input_position = 0;
    while input_position < input.len() && !errored {
        let c = input.char_at(input_position);
        // Here's the character we'll append to result, or none() to suppress it.
        let mut to_append_or_none = Some(c);
        if mode == Mode::Unquoted {
            match c {
                '\\' if !ignore_backslashes => {
                    // Backslashes (escapes) are complicated and may result in errors, or
                    // appending INTERNAL_SEPARATORs, so we have to handle them specially.
                    if let Some(escape_chars) = unescape_one(
                        &input[input_position..],
                        &mut result,
                        allow_incomplete,
                        unescape_special,
                    ) {
                        // Skip over the characters we read, minus one because the outer loop
                        // will increment it.
                        assert!(escape_chars > 0);
                        input_position += escape_chars - 1;
                    } else {
                        // A none() return indicates an error.
                        errored = true;
                    }
                    // We've already appended, don't append anything else.
                    to_append_or_none = None;
                }
                '~' if unescape_special
                    && (input_position == 0 || Some(input_position) == potential_word_start) =>
                {
                    to_append_or_none = Some(HOME_DIRECTORY);
                }
                '%' if allow_percent_self
                    && unescape_special
                    && input_position == 0
                    && input == PROCESS_EXPAND_SELF_STR =>
                // Note that this only recognizes %self if the string is literally %self.
                // %self/foo will NOT match this.
                {
                    to_append_or_none = Some(PROCESS_EXPAND_SELF);
                    input_position += PROCESS_EXPAND_SELF_STR.len() - 1; // skip over 'self's
                }
                '*' if unescape_special => {
                    // In general, this is ANY_STRING. But as a hack, if the last appended char
                    // is ANY_STRING, delete the last char and store ANY_STRING_RECURSIVE to
                    // reflect the fact that ** is the recursive wildcard.
                    if result.chars().next_back() == Some(ANY_STRING) {
                        assert!(!result.is_empty());
                        result.truncate(result.len() - 1);
                        to_append_or_none = Some(ANY_STRING_RECURSIVE);
                    } else {
                        to_append_or_none = Some(ANY_STRING);
                    }
                }
                '?' if unescape_special && !feature_test(FeatureFlag::QuestionMarkNoGlob) => {
                    to_append_or_none = Some(ANY_CHAR);
                }
                '$' if unescape_special => {
                    let is_cmdsub = input_position + 1 < input.len()
                        && input.char_at(input_position + 1) == '(';
                    if !is_cmdsub {
                        to_append_or_none = Some(VARIABLE_EXPAND);
                        vars_or_seps.push(input_position);
                    }
                }
                '{' if unescape_special => {
                    brace_count += 1;
                    to_append_or_none = Some(BRACE_BEGIN);
                    // We need to store where the brace *ends up* in the output.
                    braces.push(result.len());
                    potential_word_start = Some(input_position + 1);
                }
                '}' if unescape_special => {
                    // HACK: The completion machinery sometimes hands us partial tokens.
                    // We can't parse them properly, but it shouldn't hurt,
                    // so we don't assert here.
                    // See #4954.
                    // assert(brace_count > 0 && "imbalanced brackets are a tokenizer error, we
                    // shouldn't be able to get here");
                    brace_count -= 1;
                    to_append_or_none = Some(BRACE_END);
                    if let Some(brace) = braces.pop() {
                        // HACK: To reduce accidental use of brace expansion, treat a brace
                        // with zero or one items as literal input. See #4632. (The hack is
                        // doing it here and like this.)
                        if vars_or_seps.last().is_none_or(|i| *i < brace) {
                            result.as_char_slice_mut()[brace] = '{';
                            // We also need to turn all spaces back.
                            for i in brace + 1..result.len() {
                                if result.char_at(i) == BRACE_SPACE {
                                    result.as_char_slice_mut()[i] = ' ';
                                }
                            }
                            to_append_or_none = Some('}');
                        }
                        // Remove all seps inside the current brace pair, so if we have a
                        // surrounding pair we only get seps inside *that*.
                        if !vars_or_seps.is_empty() {
                            while vars_or_seps.last().map(|i| *i > brace).unwrap_or_default() {
                                vars_or_seps.pop();
                            }
                        }
                    }
                }
                '[' if unescape_special => {
                    to_append_or_none = Some(SLICE_BEGIN);
                }
                ']' if unescape_special => {
                    to_append_or_none = Some(SLICE_END);
                }
                ',' if unescape_special && brace_count > 0 => {
                    to_append_or_none = Some(BRACE_SEP);
                    vars_or_seps.push(input_position);
                    potential_word_start = Some(input_position + 1);
                }
                ' ' if unescape_special && brace_count > 0 => {
                    to_append_or_none = Some(BRACE_SPACE);
                }
                '\'' => {
                    mode = Mode::SingleQuotes;
                    to_append_or_none = if unescape_special {
                        Some(INTERNAL_SEPARATOR)
                    } else {
                        None
                    };
                }
                '"' => {
                    mode = Mode::DoubleQuotes;
                    to_append_or_none = if unescape_special {
                        Some(INTERNAL_SEPARATOR)
                    } else {
                        None
                    };
                }
                _ => (),
            }
        } else if mode == Mode::SingleQuotes {
            if c == '\\' {
                // A backslash may or may not escape something in single quotes.
                match input.char_at(input_position + 1) {
                    '\\' | '\'' => {
                        to_append_or_none = Some(input.char_at(input_position + 1));
                        input_position += 1; // skip over the backslash
                    }
                    '\0' => {
                        if !allow_incomplete {
                            errored = true;
                        } else {
                            // PCA this line had the following cryptic comment: 'We may ever escape
                            // a NULL character, but still appending a \ in case I am wrong.' Not
                            // sure what it means or the importance of this.
                            input_position += 1; /* Skip over the backslash */
                            to_append_or_none = Some('\\');
                        }
                    }
                    _ => {
                        // Literal backslash that doesn't escape anything! Leave things alone; we'll
                        // append the backslash itself.
                    }
                }
            } else if c == '\'' {
                to_append_or_none = if unescape_special {
                    Some(INTERNAL_SEPARATOR)
                } else {
                    None
                };
                mode = Mode::Unquoted;
            }
        } else if mode == Mode::DoubleQuotes {
            match c {
                '"' => {
                    mode = Mode::Unquoted;
                    to_append_or_none = if unescape_special {
                        Some(INTERNAL_SEPARATOR)
                    } else {
                        None
                    };
                }
                '\\' => {
                    let continuation =
                        line_continuation_length(&input.as_char_slice()[input_position..]);
                    if continuation > 0 {
                        input_position += continuation;
                        continue;
                    }
                    match input.char_at(input_position + 1) {
                        '\0' => {
                            if !allow_incomplete {
                                errored = true;
                            } else {
                                to_append_or_none = Some('\0');
                            }
                        }
                        '\\' | '$' | '"' => {
                            to_append_or_none = Some(input.char_at(input_position + 1));
                            input_position += 1; /* Skip over the backslash */
                        }
                        _ => {
                            /* Literal backslash that doesn't escape anything! Leave things alone;
                             * we'll append the backslash itself */
                        }
                    }
                }
                '$' if unescape_special => {
                    to_append_or_none = Some(VARIABLE_EXPAND_SINGLE);
                    vars_or_seps.push(input_position);
                }
                '[' if unescape_special => {
                    to_append_or_none = Some(SLICE_BEGIN);
                }
                ']' if unescape_special => {
                    to_append_or_none = Some(SLICE_END);
                }
                _ => (),
            }
        }

        // Now maybe append the char.
        if let Some(c) = to_append_or_none {
            result.push(c);
        }
        input_position += 1;
    }

    // Return the string by reference, and then success.
    if errored {
        return None;
    }
    Some(result)
}

/// Reverse the effects of `escape_string_url()`. By definition the input should consist of just
/// ASCII chars.
fn unescape_string_url(input: &wstr) -> Option<WString> {
    let mut result: Vec<u8> = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let c = input.char_at(i);
        if c > '\u{7F}' {
            return None; // invalid character means we can't decode the string
        }
        if c == '%' {
            let c1 = input.char_at(i + 1);
            if c1 == '\0' {
                return None;
            } else if c1 == '%' {
                result.push(b'%');
                i += 1;
            } else {
                let d1 = c1.to_digit(16)?;
                let c2 = input.char_at(i + 2);
                let d2 = c2.to_digit(16)?; // also fails if '\0' i.e. premature end
                result.push((16 * d1 + d2) as u8);
                i += 2;
            }
        } else {
            result.push(c as u8);
        }
        i += 1;
    }

    Some(bytes2wcstring(&result))
}

/// Reverse the effects of `escape_string_var()`. By definition the string should consist of just
/// ASCII chars.
fn unescape_string_var(input: &wstr) -> Option<WString> {
    let mut result: Vec<u8> = Vec::with_capacity(input.len());
    let mut prev_was_hex_encoded = false;
    let mut i = 0;
    while i < input.len() {
        let c = input.char_at(i);
        if c > '\u{7F}' {
            return None; // invalid character means we can't decode the string
        }
        if c == '_' {
            let c1 = input.char_at(i + 1);
            if c1 == '\0' {
                if prev_was_hex_encoded {
                    break;
                }
                return None; // found unexpected escape char at end of string
            } else if c1 == '_' {
                result.push(b'_');
                i += 1;
            } else if is_upper_hex_digit(c1) {
                let d1 = c1.to_digit(16)?;
                let c2 = input.char_at(i + 2);
                let d2 = c2.to_digit(16)?; // also fails if '\0' i.e. premature end
                result.push((16 * d1 + d2) as u8);
                i += 2;
                prev_was_hex_encoded = true;
            }
            // No "else" clause because if the first char after an underscore is not another
            // underscore or a valid hex character then the underscore is there to improve
            // readability after we've encoded a character not valid in a var name.
        } else {
            result.push(c as u8);
        }
        i += 1;
    }

    Some(bytes2wcstring(&result))
}

/// Given a string starting with a backslash, read the escape as if it is unquoted, appending
/// to result. Return the number of characters consumed, or none on error.
pub fn unescape_one(
    input: &wstr,
    result: &mut WString,
    allow_incomplete: bool,
    unescape_special: bool,
) -> Option<usize> {
    assert_eq!(input.char_at(0), '\\', "not an escape");
    let continuation = line_continuation_length(input.as_char_slice());
    if continuation > 0 {
        return Some(continuation);
    }

    // Here's the character we'll ultimately append, or none. Note that '\0' is a
    // valid thing to append.
    let mut result_char_or_none: Option<char> = None;

    let mut errored = false;
    // in_pos always tracks the next character to read
    // (and therefore the number of characters read so far)
    let mut in_pos = 1;

    // For multibyte \X sequences.
    let mut byte_buff: Vec<u8> = vec![];

    loop {
        let c = input.char_at(in_pos);
        in_pos += 1;
        match c {
            // A null character after a backslash is an error.
            '\0' => {
                // Adjust in_pos to only include the backslash.
                assert!(in_pos > 0);
                in_pos -= 1;

                // It's an error, unless we're allowing incomplete escapes.
                if !allow_incomplete {
                    errored = true;
                }
            }
            // Numeric escape sequences. No prefix means octal escape, otherwise hexadecimal.
            '0'..='7' | 'u' | 'U' | 'x' | 'X' => {
                let mut res: u64 = 0;
                let mut chars = 2;
                let mut base = 16;
                let mut byte_literal = false;
                let mut max_val = ASCII_MAX;

                match c {
                    'u' => {
                        chars = 4;
                        max_val = UCS2_MAX;
                    }
                    'U' => {
                        chars = 8;
                        // Don't exceed the largest Unicode code point - see #1107.
                        max_val = char::MAX;
                    }
                    'x' | 'X' => {
                        byte_literal = true;
                        max_val = BYTE_MAX;
                    }
                    _ => {
                        base = 8;
                        chars = 3;
                        // Note that in_pos currently is just after the first post-backslash
                        // character; we want to start our escape from there.
                        assert!(in_pos > 0);
                        in_pos -= 1;
                    }
                }

                for i in 0..chars {
                    let Some(d) = input.char_at(in_pos).to_digit(base) else {
                        // If we have no digit, this is a tokenizer error.
                        if i == 0 {
                            errored = true;
                        }
                        break;
                    };

                    res = (res * u64::from(base)) + u64::from(d);
                    in_pos += 1;
                }

                if !errored && res <= u64::from(max_val) {
                    if byte_literal {
                        // Multibyte encodings necessitate that we keep adjacent byte escapes.
                        // - `\Xc3\Xb6` is "ö", but only together.
                        // (this assumes a valid codepoint can't consist of multiple bytes
                        // that are valid on their own, which is true for UTF-8)
                        byte_buff.push(res.try_into().unwrap());
                        result_char_or_none = None;
                        if input[in_pos..].starts_with("\\X") || input[in_pos..].starts_with("\\x")
                        {
                            in_pos += 1;
                            continue;
                        }
                    } else {
                        result_char_or_none =
                            Some(char::from_u32(res.try_into().unwrap()).unwrap_or('\u{FFFD}'));
                    }
                } else {
                    errored = true;
                }
            }
            // \a means bell (alert).
            'a' => {
                result_char_or_none = Some('\x07');
            }
            // \b means backspace.
            'b' => {
                result_char_or_none = Some('\x08');
            }
            // \cX means control sequence X.
            'c' => {
                let sequence_char = u32::from(input.char_at(in_pos));
                in_pos += 1;
                // Range covers:
                //   @ABCDEFGHIJKLMNOPQRSTUVWXYZ[\]^_
                //   `abcdefghijklmnopqrstuvwxyz{|}~
                // `\c<0x7f>` (lowercase version of `\c_`) is not supported since
                // `0x7f` a control character to begin with.
                if (u32::from('@')..0x7f).contains(&sequence_char) {
                    let ctrl = char::from_u32(sequence_char % 32).unwrap();
                    result_char_or_none = Some(ctrl);
                } else {
                    errored = true;
                }
            }
            // \x1B means escape.
            'e' => {
                result_char_or_none = Some('\x1B');
            }
            // \f means form feed.
            'f' => {
                result_char_or_none = Some('\x0C');
            }
            // \n means newline.
            'n' => {
                result_char_or_none = Some('\n');
            }
            // \r means carriage return.
            'r' => {
                result_char_or_none = Some('\x0D');
            }
            // \t means tab.
            't' => {
                result_char_or_none = Some('\t');
            }
            // \v means vertical tab.
            'v' => {
                result_char_or_none = Some('\x0b');
            }
            _ => {
                if unescape_special {
                    result.push(INTERNAL_SEPARATOR);
                }
                result_char_or_none = Some(c);
            }
        }

        if errored {
            return None;
        }

        if !byte_buff.is_empty() {
            result.push_utfstr(&bytes2wcstring(&byte_buff));
        }

        break;
    }

    if let Some(c) = result_char_or_none {
        if fish_reserved_codepoint(c) {
            return None;
        }
        result.push(c);
    }

    Some(in_pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_line_continuations_preserve_other_carriage_returns() {
        for (source, expected) in [
            (L!("pre\\\r\npost"), L!("prepost")),
            (L!("\"pre\\\r\npost\""), L!("prepost")),
            (L!("'pre\\\r\npost'"), L!("pre\\\r\npost")),
            (L!("\"pre\r\npost\""), L!("pre\r\npost")),
            (L!("pre\\\rpost"), L!("pre\rpost")),
        ] {
            assert_eq!(
                unescape_string(source, UnescapeStringStyle::default()).as_deref(),
                Some(expected),
                "{source:?}"
            );
        }
    }

    mod unescape_one {
        use super::*;

        fn test_good(escaped: &wstr, expected: &wstr) {
            let mut unesc = WString::new();
            let r = unescape_one(escaped, &mut unesc, false, false);
            assert_eq!(r, Some(escaped.len()));
            assert_eq!(unesc, expected, "{escaped} -> {:?}", unesc);
        }

        fn test_bad(escaped: &wstr) {
            let mut unesc = WString::new();
            let r = unescape_one(escaped, &mut unesc, false, false);
            assert_eq!(r, None, "{escaped} -> {:?}", unesc);
        }

        #[test]
        fn control() {
            test_bad(L!("\\c?"));
            test_good(L!("\\c@"), L!("\x00"));
            test_good(L!("\\cA"), L!("\x01"));
            test_good(L!("\\c_"), L!("\x1f"));
            test_good(L!("\\c`"), L!("\x00"));
            test_good(L!("\\ca"), L!("\x01"));
            test_bad(L!("\\c\x7f"));
            test_bad(L!("\\c\u{0080}"));
        }
    }
}
