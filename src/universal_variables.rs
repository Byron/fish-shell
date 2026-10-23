//! The Fish universal-variable file format, independent of environment and storage backends.
use fish_wcstringutil::escape::{UnescapeFlags, UnescapeStringStyle, unescape_string};
use fish_wcstringutil::{LineIterator, join_strings};
use fish_widestring::{decode_byte_from_char, prelude::*};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(non_camel_case_types)]
pub(crate) enum Format {
    Fish_2_x,
    Fish_3_0,
    Future,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Flags {
    pub exported: bool,
    pub pathvar: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub values: Vec<WString>,
    pub flags: Flags,
}

impl Entry {
    fn new_vec(values: Vec<WString>, flags: Flags) -> Self {
        Self { values, flags }
    }
    fn flags(&self) -> Flags {
        self.flags
    }
    fn as_list(&self) -> &[WString] {
        &self.values
    }
}
pub(crate) type Table = BTreeMap<WString, Entry>;

pub(crate) struct Codec;
impl Codec {
    /// Populate a variable table `out_vars` from a `s` string.
    /// Return the format of the file that we read.
    pub(crate) fn populate_variables(
        s: &[u8],
        out_vars: &mut Table,
        mut diagnostic: impl FnMut(&wstr),
    ) -> Format {
        // Decide on the format.
        let format = Self::format_for_contents(s);

        let iter = LineIterator::new(s);
        let mut wide_line = WString::new();
        let mut storage = WString::new();
        for line in iter {
            // Skip empties and constants.
            if line.is_empty() || line[0] == b'#' {
                continue;
            }

            // Convert to UTF8.
            wide_line.clear();
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            // TODO: investigate whether this should use str2wcstring
            wide_line = WString::from_str(line);

            match format {
                Format::Fish_2_x => {
                    Self::parse_message_2x_internal(
                        &wide_line,
                        out_vars,
                        &mut storage,
                        &mut diagnostic,
                    );
                }
                Format::Fish_3_0 => {
                    Self::parse_message_30_internal(
                        &wide_line,
                        out_vars,
                        &mut storage,
                        &mut diagnostic,
                    );
                }
                // For future formats, just try with the most recent one.
                Format::Future => {
                    Self::parse_message_30_internal(
                        &wide_line,
                        out_vars,
                        &mut storage,
                        &mut diagnostic,
                    );
                }
            }
        }
        format
    }

    /// Guess a file format.
    /// Return the format corresponding to file contents `s`.
    pub(crate) fn format_for_contents(s: &[u8]) -> Format {
        // Walk over leading comments, looking for one like '# version'
        let iter = LineIterator::new(s);
        for line in iter {
            if line.is_empty() {
                continue;
            }
            if line[0] != b'#' {
                // Exhausted leading comments.
                break;
            }
            // Match scanf's optional whitespace while avoiding C-string conversion:
            // malformed files containing NUL bytes must never panic the shell.
            let mut rest = &line[1..];
            while rest
                .first()
                .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == 0x0b)
            {
                rest = &rest[1..];
            }
            let Some(rest) = rest.strip_prefix(b"VERSION:") else {
                continue;
            };
            let version = rest
                .split(|byte| byte.is_ascii_whitespace() || *byte == 0x0b)
                .find(|part| !part.is_empty());
            let Some(version) = version else {
                continue;
            };
            return if version == UVARS_VERSION_3_0 {
                Format::Fish_3_0
            } else {
                Format::Future
            };
        }
        // No version found, assume 2.x
        Format::Fish_2_x
    }

    /// Serialize a variable list.
    pub(crate) fn serialize_with_vars(vars: &Table, mut diagnostic: impl FnMut(&wstr)) -> Vec<u8> {
        let mut contents = vec![];
        contents.extend_from_slice(SAVE_MSG);
        contents.extend_from_slice(b"# VERSION: ");
        contents.extend_from_slice(UVARS_VERSION_3_0);
        contents.push(b'\n');

        // BTreeMap preserves the existing sorted file representation.
        vars.iter().for_each(|(k, v)| {
            // Append the entry. Note that append_file_entry may fail,
            // but that only affects one variable; soldier on.
            if !append_file_entry(v.flags(), k, &encode_serialized(v.as_list()), &mut contents) {
                diagnostic(k);
            }
        });

        contents
    }

    fn populate_1_variable(
        input: &wstr,
        flags: Flags,
        vars: &mut Table,
        storage: &mut WString,
    ) -> bool {
        let s = skip_spaces(input);
        let Some(colon) = s.chars().position(|c| c == ':') else {
            return false;
        };

        // Parse out the value into storage, and decode it into a variable.
        storage.clear();
        let Some(unescaped) = unescape_string(
            &s[colon + 1..],
            UnescapeStringStyle::Script(UnescapeFlags::default()),
        ) else {
            return false;
        };
        *storage = unescaped;
        let var = Entry::new_vec(decode_serialized(&*storage), flags);

        // Parse out the key and write into the map.
        *storage = s[..colon].to_owned();
        let key = &*storage;
        (*vars).insert(key.clone(), var);
        true
    }
    /// Parse message msg per fish 3.0 format.
    fn parse_message_30_internal(
        msg: &wstr,
        vars: &mut Table,
        storage: &mut WString,
        diagnostic: &mut impl FnMut(&wstr),
    ) {
        use fish3_uvars as f3;
        if msg.starts_with(L!("#")) {
            return;
        }

        let mut cursor = msg;
        if !r#match(&mut cursor, f3::SETUVAR) {
            diagnostic(msg);
            return;
        }
        // Parse out flags.
        let mut flags = Flags::default();
        loop {
            cursor = skip_spaces(cursor);
            if cursor.char_at(0) != '-' {
                break;
            }
            if r#match(&mut cursor, f3::EXPORT) {
                flags.exported = true;
            } else if r#match(&mut cursor, f3::PATH) {
                flags.pathvar = true;
            } else {
                // Skip this unknown flag, for future proofing.
                while !cursor.is_empty() && !matches!(cursor.char_at(0), ' ' | '\t') {
                    cursor = &cursor[1..];
                }
            }
        }

        // Populate the variable with these flags.
        if !Self::populate_1_variable(cursor, flags, vars, storage) {
            diagnostic(msg);
        }
    }

    /// Parse message msg per fish 2.x format.
    fn parse_message_2x_internal(
        msg: &wstr,
        vars: &mut Table,
        storage: &mut WString,
        diagnostic: &mut impl FnMut(&wstr),
    ) {
        use fish2x_uvars as f2x;
        let mut cursor = msg;

        if cursor.char_at(0) == '#' {
            return;
        }
        let mut flags = Flags::default();
        if r#match(&mut cursor, f2x::SET_EXPORT) {
            flags.exported = true;
        } else if r#match(&mut cursor, f2x::SET) {
        } else {
            diagnostic(msg);
            return;
        }

        if !Self::populate_1_variable(cursor, flags, vars, storage) {
            diagnostic(msg);
        }
    }
}

/// Small note about not editing ~/.fishd manually. Inserted at the top of all .fishd files.
const SAVE_MSG: &[u8] = b"# This file contains fish universal variable definitions.\n";

/// Version for fish 3.0
const UVARS_VERSION_3_0: &[u8] = b"3.0";

// Maximum file size we'll read.
pub(crate) const MAX_READ_SIZE: usize = 16 * 1024 * 1024;

// Fields used in fish 2.x uvars.

mod fish2x_uvars {
    pub const SET: &[u8] = b"SET";
    pub const SET_EXPORT: &[u8] = b"SET_EXPORT";
}
// Fields used in fish 3.0 uvars
mod fish3_uvars {
    pub const SETUVAR: &[u8] = b"SETUVAR";
    pub const EXPORT: &[u8] = b"--export";
    pub const PATH: &[u8] = b"--path";
}

/// Test if the message msg contains the command cmd.
/// On success, updates the cursor to just past the command.
fn r#match(inout_cursor: &mut &wstr, cmd: &[u8]) -> bool {
    let cursor = *inout_cursor;
    if !cmd
        .iter()
        .copied()
        .map(char::from)
        .eq(cursor.chars().take(cmd.len()))
    {
        return false;
    }
    let len = cmd.len();
    if cursor.len() != len && !matches!(cursor.char_at(len), ' ' | '\t') {
        return false;
    }
    *inout_cursor = &cursor[len..];
    true
}

/// The universal variable format has some funny escaping requirements; here we try to be safe.
fn is_universal_safe_to_encode_directly(c: char) -> bool {
    if !(32..=128).contains(&u32::from(c)) {
        return false;
    }

    c.is_alphanumeric() || matches!(c, '/' | '_')
}

/// Escape specified string.
fn full_escape(input: &wstr) -> WString {
    let mut out = WString::new();
    for c in input.chars() {
        if is_universal_safe_to_encode_directly(c) {
            out.push(c);
        } else if c.is_ascii() {
            out.push_str(&format!("\\x{:02x}", u32::from(c)));
        } else if let Some(encoded_byte) = decode_byte_from_char(c) {
            out.push_str(&format!("\\x{:02x}", u32::from(encoded_byte)));
        } else if u32::from(c) < 65536 {
            out.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            out.push_str(&format!("\\U{:08x}", u32::from(c)));
        }
    }
    out
}

/// Converts input to UTF-8 and appends it to receiver.
fn append_utf8(input: &wstr, receiver: &mut Vec<u8>) {
    // Notably we convert between wide and narrow strings without decoding our private-use
    // characters.
    receiver.extend_from_slice(input.to_string().as_bytes());
}

/// Creates a file entry like "SET fish_color_cwd:FF0". Appends the result to *result (as UTF8).
/// Returns true on success. storage may be used for temporary storage, to avoid allocations.
fn append_file_entry(flags: Flags, key_in: &wstr, val_in: &wstr, result: &mut Vec<u8>) -> bool {
    use fish3_uvars as f3;

    // Record the length on entry, in case we need to back up.
    let mut success = true;
    let result_length_on_entry = result.len();

    // Append SETVAR header.
    result.extend_from_slice(f3::SETUVAR);
    result.push(b' ');

    // Append flags.
    if flags.exported {
        result.extend_from_slice(f3::EXPORT);
        result.push(b' ');
    }
    if flags.pathvar {
        result.extend_from_slice(f3::PATH);
        result.push(b' ');
    }

    // Append variable name like "fish_color_cwd".
    if key_in.is_empty() || !key_in.chars().all(crate::common::valid_var_name_char) {
        success = false;
    }
    if success {
        append_utf8(key_in, result);
    }

    // Append ":".
    if success {
        result.push(b':');
    }

    // Append value.
    if success {
        append_utf8(&full_escape(val_in), result);
    }

    // Append newline.
    if success {
        result.push(b'\n');
    }

    // Don't modify result on failure. It's sufficient to simply resize it since all we ever did was
    // append to it.
    if !success {
        result.truncate(result_length_on_entry);
    }

    success
}

/// Encoding of a null string.
const ENV_NULL: &wstr = L!("\x1d");

/// Character used to separate arrays in universal variables file.
/// This is 30, the ASCII record separator.
const UVAR_ARRAY_SEP: char = '\x1e';

/// Decode a serialized universal variable value into a list.
fn decode_serialized(val: &wstr) -> Vec<WString> {
    if val == ENV_NULL {
        return vec![];
    }
    val.split(UVAR_ARRAY_SEP).map(|v| v.to_owned()).collect()
}

/// Decode a a list into a serialized universal variable value.
fn encode_serialized(vals: &[WString]) -> WString {
    if vals.is_empty() {
        return ENV_NULL.to_owned();
    }
    join_strings(vals, UVAR_ARRAY_SEP)
}

fn skip_spaces(mut s: &wstr) -> &wstr {
    while s.starts_with(L!(" ")) || s.starts_with(L!("\t")) {
        s = &s[1..];
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use Entry as EnvVar;
    use Flags as EnvVarFlags;
    use Table as VarTable;
    use fish_widestring::{ENCODE_DIRECT_BASE, char_offset};

    impl Entry {
        fn new(value: WString, flags: Flags) -> Self {
            Self::new_vec(vec![value], flags)
        }
    }

    #[test]
    fn test_universal_output() {
        let flag_export = EnvVarFlags {
            exported: true,
            ..Default::default()
        };
        let flag_pathvar = EnvVarFlags {
            pathvar: true,
            ..Default::default()
        };
        let flag_export_pathvar = EnvVarFlags {
            exported: true,
            pathvar: true,
        };

        let mut vars = VarTable::new();
        vars.insert(
            L!("varA").to_owned(),
            EnvVar::new_vec(
                vec![L!("ValA1").to_owned(), L!("ValA2").to_owned()],
                EnvVarFlags::default(),
            ),
        );
        vars.insert(
            L!("varB").to_owned(),
            EnvVar::new_vec(vec![L!("ValB1").to_owned()], flag_export),
        );
        vars.insert(
            L!("varC").to_owned(),
            EnvVar::new_vec(vec![L!("ValC1").to_owned()], EnvVarFlags::default()),
        );
        vars.insert(
            L!("varD").to_owned(),
            EnvVar::new_vec(vec![L!("ValD1").to_owned()], flag_export_pathvar),
        );
        vars.insert(
            L!("varE").to_owned(),
            EnvVar::new_vec(
                vec![L!("ValE1").to_owned(), L!("ValE2").to_owned()],
                flag_pathvar,
            ),
        );
        vars.insert(
            L!("varF").to_owned(),
            EnvVar::new_vec(
                vec![WString::from_chars([char_offset(ENCODE_DIRECT_BASE, 0xfc)])],
                EnvVarFlags::default(),
            ),
        );

        let text = Codec::serialize_with_vars(&vars, |_| {});
        let expected = concat!(
            "# This file contains fish universal variable definitions.\n",
            "# VERSION: 3.0\n",
            "SETUVAR varA:ValA1\\x1eValA2\n",
            "SETUVAR --export varB:ValB1\n",
            "SETUVAR varC:ValC1\n",
            "SETUVAR --export --path varD:ValD1\n",
            "SETUVAR --path varE:ValE1\\x1eValE2\n",
            "SETUVAR varF:\\xfc\n",
        )
        .as_bytes();
        assert_eq!(text, expected);
    }

    #[test]
    fn test_universal_parsing() {
        let input = concat!(
            "# This file contains fish universal variable definitions.\n",
            "# VERSION: 3.0\n",
            "SETUVAR varA:ValA1\\x1eValA2\n",
            "SETUVAR --export varB:ValB1\n",
            "SETUVAR --nonsenseflag varC:ValC1\n",
            "SETUVAR --export --path varD:ValD1\n",
            "SETUVAR --path --path varE:ValE1\\x1eValE2\n",
        )
        .as_bytes();

        let flag_export = EnvVarFlags {
            exported: true,
            ..Default::default()
        };
        let flag_pathvar = EnvVarFlags {
            pathvar: true,
            ..Default::default()
        };
        let flag_export_pathvar = EnvVarFlags {
            exported: true,
            pathvar: true,
        };

        let mut vars = VarTable::new();

        vars.insert(
            L!("varA").to_owned(),
            EnvVar::new_vec(
                vec![L!("ValA1").to_owned(), L!("ValA2").to_owned()],
                EnvVarFlags::default(),
            ),
        );
        vars.insert(
            L!("varB").to_owned(),
            EnvVar::new_vec(vec![L!("ValB1").to_owned()], flag_export),
        );
        vars.insert(
            L!("varC").to_owned(),
            EnvVar::new_vec(vec![L!("ValC1").to_owned()], EnvVarFlags::default()),
        );
        vars.insert(
            L!("varD").to_owned(),
            EnvVar::new_vec(vec![L!("ValD1").to_owned()], flag_export_pathvar),
        );
        vars.insert(
            L!("varE").to_owned(),
            EnvVar::new_vec(
                vec![L!("ValE1").to_owned(), L!("ValE2").to_owned()],
                flag_pathvar,
            ),
        );

        let mut parsed_vars = VarTable::new();
        Codec::populate_variables(input, &mut parsed_vars, |_| {});
        assert_eq!(vars, parsed_vars);
    }

    #[test]
    fn test_universal_parsing_legacy() {
        let input = concat!(
            "# This file contains fish universal variable definitions.\n",
            "SET varA:ValA1\\x1eValA2\n",
            "SET_EXPORT varB:ValB1\n",
        )
        .as_bytes();

        let mut vars = VarTable::new();
        vars.insert(
            L!("varA").to_owned(),
            EnvVar::new_vec(
                vec![L!("ValA1").to_owned(), L!("ValA2").to_owned()],
                EnvVarFlags::default(),
            ),
        );
        vars.insert(
            L!("varB").to_owned(),
            EnvVar::new(
                L!("ValB1").to_owned(),
                EnvVarFlags {
                    exported: true,
                    ..Default::default()
                },
            ),
        );

        let mut parsed_vars = VarTable::new();
        Codec::populate_variables(input, &mut parsed_vars, |_| {});
        assert_eq!(vars, parsed_vars);
    }

    #[test]
    fn test_universal_formats() {
        macro_rules! validate {
            ( $version_line:literal, $expected_format:expr ) => {
                assert_eq!(Codec::format_for_contents($version_line), $expected_format);
            };
        }
        validate!(b"# VERSION: 3.0", Format::Fish_3_0);
        validate!(b"#\x0bVERSION:\x0b3.0", Format::Fish_3_0);
        validate!(b"# version: 3.0", Format::Fish_2_x);
        validate!(b"# blah blahVERSION: 3.0", Format::Fish_2_x);
        validate!(b"stuff\n# blah blahVERSION: 3.0", Format::Fish_2_x);
        validate!(b"# blah\n# VERSION: 3.0", Format::Fish_3_0);
        validate!(b"# blah\n#VERSION: 3.0", Format::Fish_3_0);
        validate!(b"# blah\n#VERSION:3.0", Format::Fish_3_0);
        validate!(b"# blah\n#VERSION:3.1", Format::Future);
    }

    #[test]
    fn future_versions_and_embedded_nul_are_never_writable() {
        for content in [b"# VERSION: 3.0\0future".as_slice(), b"# VERSION: 100.0"] {
            assert_eq!(Codec::format_for_contents(content), Format::Future);
        }
    }

    #[test]
    fn empty_lists_unicode_and_multiline_values_roundtrip() {
        let entries = Table::from([
            (
                L!("zero").to_owned(),
                Entry::new_vec(vec![], Flags::default()),
            ),
            (
                L!("empty").to_owned(),
                Entry::new_vec(vec![WString::new()], Flags::default()),
            ),
            (
                L!("unicode").to_owned(),
                Entry::new_vec(
                    vec![L!("文😀\n\t").to_owned(), L!("two words").to_owned()],
                    Flags {
                        exported: true,
                        pathvar: true,
                    },
                ),
            ),
        ]);
        let bytes = Codec::serialize_with_vars(&entries, |_| panic!("invalid key"));
        let mut decoded = Table::new();
        Codec::populate_variables(&bytes, &mut decoded, |_| panic!("invalid entry"));
        assert_eq!(decoded, entries);
    }
}
