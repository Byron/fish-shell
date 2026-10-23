//! Fish `set` variable indexes, shared by both environment backends.
use fish_widestring::prelude::*;

use crate::wutil::wcstoi;

#[derive(Debug)]
pub(crate) struct InvalidIndex(pub WString);

// Like ordinary expansion, index expansion must have a finite allocation budget.
pub(crate) const MAX_INDEX_COUNT: usize = 1_000_000;

pub(crate) fn variable_name(argument: &wstr) -> &wstr {
    argument
        .find_char('[')
        .map_or(argument, |open| &argument[..open])
}

/// An empty [] has the historical meaning of no indexes at all.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn has_indexes(argument: &wstr) -> bool {
    argument
        .find_char('[')
        .is_some_and(|open| argument.char_at(open + 1) != ']')
}

/// Expand one-based indexes, including negative indexes and inclusive ranges.
pub(crate) fn parse(argument: &wstr, length: usize) -> Result<Vec<isize>, InvalidIndex> {
    let Some(open) = argument.find_char('[') else {
        return Ok(Vec::new());
    };
    let mut cursor = argument.slice_from(open + 1);
    let mut indexes = Vec::new();
    let convert = |index: isize| -> Result<isize, InvalidIndex> {
        if index >= 0 {
            return Ok(index);
        }
        isize::try_from(length)
            .ok()
            .and_then(|length| length.checked_add(index)?.checked_add(1))
            .ok_or_else(|| InvalidIndex(argument.to_owned()))
    };
    while cursor.char_at(0) != ']' {
        let first = if indexes.is_empty() && cursor.starts_with(L!("..")) {
            1
        } else {
            read_index(&mut cursor)?
        };
        let first = convert(first)?;
        if cursor.starts_with(L!("..")) {
            cursor = cursor.slice_from(2);
            let last = if indexes.is_empty() && cursor.char_at(0) == ']' {
                -1
            } else {
                read_index(&mut cursor)?
            };
            let last = convert(last)?;
            let count = first
                .abs_diff(last)
                .checked_add(1)
                .ok_or_else(|| InvalidIndex(cursor.to_owned()))?;
            if count > MAX_INDEX_COUNT - indexes.len() {
                return Err(InvalidIndex(argument.to_owned()));
            }
            if first < last {
                indexes.extend(first..=last);
            } else {
                indexes.extend((last..=first).rev());
            }
        } else {
            if indexes.len() == MAX_INDEX_COUNT {
                return Err(InvalidIndex(argument.to_owned()));
            }
            indexes.push(first);
        }
    }
    Ok(indexes)
}

fn read_index(cursor: &mut &wstr) -> Result<isize, InvalidIndex> {
    let mut consumed = 0;
    let index = wcstoi::wcstoi_partial(*cursor, wcstoi::Options::default(), &mut consumed)
        .map_err(|_| InvalidIndex(cursor.slice_from(consumed.min(cursor.len())).to_owned()))?;
    *cursor = cursor.slice_from(consumed);
    while !cursor.is_empty() && cursor.char_at(0).is_whitespace() {
        *cursor = cursor.slice_from(1);
    }
    Ok(index)
}

/// Remove each original position once, preserving the order of survivors.
pub(crate) fn erased_at_indexes<T>(mut input: Vec<T>, mut indexes: Vec<isize>) -> Vec<T> {
    indexes.sort_by_key(|&index| std::cmp::Reverse(index));
    indexes.dedup();
    for index in indexes {
        let Ok(index) = usize::try_from(index) else {
            continue;
        };
        if index > 0 && index <= input.len() {
            input.remove(index - 1);
        }
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_index_ranges_and_legacy_empty_brackets() {
        for (argument, expected) in [
            ("values", vec![]),
            ("values[]", vec![]),
            ("values[2 2 -1]", vec![2, 2, 4]),
            ("values[0 99 -9]", vec![0, 99, -4]),
            ("values[2..]", vec![2, 3, 4]),
            ("values[..2]", vec![1, 2]),
            ("values[3..1]", vec![3, 2, 1]),
            ("values[1 .. 2]", vec![1, 2]),
            ("values[0x2 03]suffix", vec![2, 3]),
        ] {
            let argument = WString::from(argument);
            assert_eq!(variable_name(&argument), L!("values"));
            assert_eq!(parse(&argument, 4).unwrap(), expected);
            assert_eq!(has_indexes(&argument), !expected.is_empty());
        }
    }

    #[test]
    fn malformed_overflowing_and_unbounded_indexes_are_errors() {
        for argument in [
            "v[",
            "v[ ]",
            "v[2 1..]",
            "v[1..bad]",
            "v[9999999999999999999999]",
            "v[1..9223372036854775807]",
        ] {
            assert!(parse(&WString::from(argument), 3).is_err(), "{argument}");
        }
    }

    #[test]
    fn index_erasure_uses_original_positions_and_ignores_out_of_bounds() {
        assert_eq!(
            erased_at_indexes(vec!["a", "b", "c", "d"], vec![2, 2, 4, 0, -1, 99]),
            ["a", "c"]
        );
    }
}
