//! Shared `read` field splitting, independent of streams and variable scopes.
use crate::tokenizer::{TokFlags, Tokenizer};
use fish_wcstringutil::escape::{UnescapeStringStyle, unescape_string};
use fish_wcstringutil::{split_about, split_string_tok};
use fish_widestring::{WString, wstr};

pub(crate) enum SplitMode<'a> {
    Delimiter(&'a wstr),
    Ifs(&'a wstr),
    Tokens { unescape: bool },
}

/// Return values for each assigned variable. Unassigned trailing variables are
/// omitted; the caller clears them or reads another record in `--line` mode.
pub(crate) fn split(
    input: &wstr,
    mode: SplitMode<'_>,
    array: bool,
    count: usize,
) -> Vec<Vec<WString>> {
    assert!(count > 0);
    if let SplitMode::Tokens { unescape } = mode {
        let mut tokenizer = Tokenizer::new(
            input,
            TokFlags {
                accept_unfinished: true,
                argument_list: true,
                ..Default::default()
            },
        );
        let text = |raw: &wstr| {
            if unescape {
                unescape_string(raw, UnescapeStringStyle::default())
                    .unwrap_or_else(|| raw.to_owned())
            } else {
                raw.to_owned()
            }
        };
        if array {
            let mut tokens = Vec::new();
            while let Some(token) = tokenizer.next() {
                tokens.push(text(tokenizer.text_of(&token)));
            }
            return vec![tokens];
        }
        let mut values = Vec::new();
        for _ in 1..count {
            let Some(token) = tokenizer.next() else { break };
            values.push(vec![text(tokenizer.text_of(&token))]);
        }
        if let Some(token) = tokenizer.next() {
            values.push(vec![input[token.offset as usize..].to_owned()]);
        }
        return values;
    }
    let (delimiter, is_ifs) = match mode {
        SplitMode::Delimiter(value) => (value, false),
        SplitMode::Ifs(value) => (value, true),
        SplitMode::Tokens { .. } => unreachable!(),
    };
    let values = if delimiter.is_empty() {
        let mut values = Vec::new();
        for (index, character) in input.chars().enumerate() {
            if array || index + 1 < count {
                values.push(WString::from_chars([character]));
            } else {
                values.push(input[index..].to_owned());
                break;
            }
        }
        values
    } else if is_ifs {
        let mut values: Vec<_> = split_string_tok(input, delimiter, (!array).then_some(count))
            .into_iter()
            .map(wstr::to_owned)
            .collect();
        if !array {
            values.resize(count, WString::new());
        }
        values
    } else {
        split_about(
            input,
            delimiter,
            if array { usize::MAX } else { count - 1 },
            false,
        )
        .into_iter()
        .map(wstr::to_owned)
        .collect()
    };
    if array {
        vec![values]
    } else {
        values.into_iter().map(|value| vec![value]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fish_widestring::L;

    #[test]
    fn tokenized_last_variable_retains_the_unexpanded_remainder() {
        let result = split(
            L!("'one two' $value (echo unsafe)"),
            SplitMode::Tokens { unescape: true },
            false,
            2,
        );
        assert_eq!(
            result,
            vec![
                vec![WString::from("one two")],
                vec![WString::from("$value (echo unsafe)")]
            ]
        );
    }

    #[test]
    fn delimiters_and_ifs_keep_distinct_empty_field_rules() {
        let explicit = split(L!("a::b:"), SplitMode::Delimiter(L!(":")), true, 1);
        assert_eq!(
            explicit[0]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["a", "", "b", ""]
        );
        let ifs = split(L!("a::b:"), SplitMode::Ifs(L!(":")), true, 1);
        assert_eq!(
            ifs[0].iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["a", "b"]
        );
        let chars = split(L!("文😀ab"), SplitMode::Delimiter(L!("")), false, 2);
        assert_eq!(
            chars,
            vec![vec![WString::from("文")], vec![WString::from("😀ab")]]
        );
    }
}
