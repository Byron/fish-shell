//! Platform-independent field selection and character ranges for `string`.
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldError {
    Number,
    Range,
    Field,
    Limit,
}

/// Parse one-based fields and inclusive ranges, preserving order and repetition.
/// A caller may bound the expanded selection before any range is allocated.
pub(crate) fn parse_fields(value: &str, limit: usize) -> Result<Vec<usize>, FieldError> {
    let number = |value: &str| value.trim().parse::<i32>().map_err(|_| FieldError::Number);
    let mut fields = Vec::new();
    for field in value.split(',') {
        let range: Vec<_> = field.split('-').collect();
        let (start, end) = match range.as_slice() {
            [start, end] => {
                let start = number(start)?;
                let end = number(end)?;
                if start <= 0 || end <= 0 {
                    return Err(FieldError::Range);
                }
                (start as usize - 1, end as usize - 1)
            }
            _ => {
                let field = number(field)?;
                if field <= 0 {
                    return Err(FieldError::Field);
                }
                (field as usize - 1, field as usize - 1)
            }
        };
        let count = start.abs_diff(end) + 1;
        if count > limit.saturating_sub(fields.len()) {
            return Err(FieldError::Limit);
        }
        if start <= end {
            fields.extend(start..=end);
        } else {
            fields.extend((end..=start).rev());
        }
    }
    Ok(fields)
}

/// Fish's one-based start and end options count Unicode characters. Negative
/// ends omit characters from the tail; a negative start counts back from it.
pub(crate) fn substring_range(
    size: usize,
    start: Option<i64>,
    end: Option<i64>,
    length: Option<usize>,
) -> Range<usize> {
    let backwards = |n: i64| size.saturating_sub(n.unsigned_abs().min(usize::MAX as u64) as usize);
    let start = match start.unwrap_or(1) {
        n @ 1.. => (n as usize - 1).min(size),
        0 => 0,
        n => backwards(n),
    };
    let count = length.or_else(|| {
        end.map(|n| {
            let end = if n > 0 { n as usize } else { backwards(n) };
            end.saturating_sub(start)
        })
    });
    start..start.saturating_add(count.unwrap_or(size)).min(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_keep_ranges_repetition_and_order() {
        assert_eq!(
            parse_fields("1-3,5,9-7,2", 9),
            Ok(vec![0, 1, 2, 4, 8, 7, 6, 1])
        );
        assert_eq!(parse_fields(" +2 , 3-1 ", 4), Ok(vec![1, 2, 1, 0]));
        assert_eq!(
            parse_fields("1-2147483647", 100_000),
            Err(FieldError::Limit)
        );
        assert_eq!(parse_fields("1,2", 1), Err(FieldError::Limit));
        assert_eq!(parse_fields("0", 10), Err(FieldError::Field));
        assert_eq!(parse_fields("1-0", 10), Err(FieldError::Range));
        for value in ["", "-1", "1--2", "1a", "2147483648"] {
            assert_eq!(parse_fields(value, 10), Err(FieldError::Number), "{value}");
        }
    }

    #[test]
    fn substring_limits_are_clamped_without_overflow() {
        assert_eq!(substring_range(5, Some(2), Some(-2), None), 1..3);
        assert_eq!(substring_range(5, None, Some(-4), None), 0..1);
        assert_eq!(substring_range(5, Some(4), Some(2), None), 3..3);
        assert_eq!(
            substring_range(5, Some(i64::MIN), None, Some(usize::MAX)),
            0..5
        );
    }
}
