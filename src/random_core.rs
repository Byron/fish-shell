//! Random requests and sampling shared by Fish's POSIX and native adapters.
use fish_wgetopt::{ArgType::NoArgument, WGetopter, WOption, wopt};
use fish_widestring::prelude::*;
use rand::{Rng as _, RngExt as _, SeedableRng as _};

#[cfg(windows)]
#[path = "wutil/errors.rs"]
mod number_errors;
#[cfg(windows)]
pub use number_errors::Error;
#[cfg(windows)]
#[path = "wutil/wcstoi.rs"]
#[allow(dead_code)]
mod wcstoi;
#[cfg(not(windows))]
use crate::wutil::{fish_wcstol, fish_wcstoul};
#[cfg(windows)]
use wcstoi::{fish_wcstol, fish_wcstoul};

// These are SmallRng's current engines, exposed directly so native workers can serialize them.
#[cfg(any(target_pointer_width = "16", target_pointer_width = "32"))]
pub(crate) use rand::rngs::Xoshiro128PlusPlus as Engine;
#[cfg(target_pointer_width = "64")]
pub(crate) use rand::rngs::Xoshiro256PlusPlus as Engine;

pub(crate) fn seeded(seed: u64) -> Engine {
    Engine::seed_from_u64(seed)
}

pub(crate) fn fresh() -> Engine {
    seeded(rand::rng().next_u64())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Request {
    Help,
    Seed(u64),
    Draw(Draw),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Draw {
    Number {
        start: i64,
        step: u64,
        intervals: u64,
    },
    Choice {
        first: usize,
        count: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Value {
    Number(i64),
    Argument(usize),
}

impl Draw {
    pub(crate) fn sample(self, engine: &mut Engine) -> Value {
        match self {
            Self::Choice { first, count } => Value::Argument(first + engine.random_range(0..count)),
            Self::Number {
                start,
                step,
                intervals,
            } => {
                let selected = engine.random_range(0..=intervals);
                // Parsing bounds intervals * step by the distance between two valid i64 values.
                Value::Number(start.checked_add_unsigned(selected * step).unwrap())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ParseError<'a> {
    MissingOptionArgument(&'a wstr),
    UnexpectedOptionArgument(&'a wstr),
    UnknownOption(&'a wstr),
    InvalidInteger(&'a wstr),
    NoChoices,
    ZeroStep,
    TooManyArguments,
}

impl ParseError<'_> {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn status(self) -> i32 {
        if self == Self::TooManyArguments { 1 } else { 2 }
    }
}

impl std::fmt::Display for ParseError<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingOptionArgument(arg) => {
                write!(formatter, "{arg}: option requires an argument")
            }
            Self::UnexpectedOptionArgument(arg) => {
                write!(formatter, "{arg}: option does not take an argument")
            }
            Self::UnknownOption(arg) => write!(formatter, "{arg}: unknown option"),
            Self::InvalidInteger(arg) => write!(formatter, "{arg}: invalid integer"),
            Self::NoChoices => formatter.write_str("nothing to choose from"),
            Self::ZeroStep => formatter.write_str("STEP must be a positive integer"),
            Self::TooManyArguments => formatter.write_str("too many arguments"),
        }
    }
}

/// Parse the complete argv, including the builtin name. Choice results retain indices into argv.
pub(crate) fn parse<'args>(argv: &mut [&'args wstr]) -> Result<Request, ParseError<'args>> {
    const LONG_OPTIONS: &[WOption] = &[wopt(L!("help"), NoArgument, 'h')];
    let mut parser = WGetopter::new(L!("+h"), LONG_OPTIONS, argv);
    if let Some(option) = parser.next_opt() {
        return match option {
            'h' => Ok(Request::Help),
            ':' => Err(ParseError::MissingOptionArgument(
                argv[parser.wopt_index - 1],
            )),
            ';' => Err(ParseError::UnexpectedOptionArgument(
                argv[parser.wopt_index - 1],
            )),
            '?' => Err(ParseError::UnknownOption(argv[parser.wopt_index - 1])),
            _ => unreachable!("invalid random option"),
        };
    }
    let first = parser.wopt_index;
    let operands = &argv[first..];
    if operands.first() == Some(&L!("choice")) {
        if operands.len() == 1 {
            return Err(ParseError::NoChoices);
        }
        return Ok(Request::Draw(Draw::Choice {
            first: first + 1,
            count: operands.len() - 1,
        }));
    }
    let signed = |value| fish_wcstol(value).map_err(|_| ParseError::InvalidInteger(value));
    let (start, step, end) = match operands {
        [] => (0, 1, 32767),
        [seed] => return Ok(Request::Seed(signed(seed)? as u64)),
        [start, end] => (signed(start)?, 1, signed(end)?),
        [start, step, end] => {
            let start = signed(start)?;
            let step = fish_wcstoul(step).map_err(|_| ParseError::InvalidInteger(step))?;
            if step == 0 {
                return Err(ParseError::ZeroStep);
            }
            (start, step, signed(end)?)
        }
        _ => return Err(ParseError::TooManyArguments),
    };
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    // abs_diff handles the entire i64 range without signed overflow.
    Ok(Request::Draw(Draw::Number {
        start,
        step,
        intervals: end.abs_diff(start) / step,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::SmallRng;

    fn request(argv: &[&'static wstr]) -> Result<Request, ParseError<'static>> {
        parse(&mut argv.to_vec())
    }

    #[test]
    fn named_engine_preserves_smallrng_seeded_sequences() {
        for seed in [0, 1, 42, i64::MAX as u64, i64::MIN as u64, u64::MAX] {
            let mut previous = SmallRng::seed_from_u64(seed);
            let mut shared = seeded(seed);
            for _ in 0..256 {
                assert_eq!(shared.next_u64(), previous.next_u64(), "seed {seed}");
            }
            for _ in 0..64 {
                assert_eq!(
                    Draw::Choice { first: 2, count: 7 }.sample(&mut shared),
                    Value::Argument(2 + previous.random_range(0..7))
                );
                assert_eq!(
                    Draw::Number {
                        start: -100,
                        step: 3,
                        intervals: 66
                    }
                    .sample(&mut shared),
                    Value::Number(-100 + previous.random_range(0..=66u64) as i64 * 3)
                );
            }
        }
    }

    #[test]
    fn parses_seeds_choices_and_ranges_with_fish_option_ordering() {
        assert_eq!(
            request(&[L!("random")]),
            Ok(Request::Draw(Draw::Number {
                start: 0,
                step: 1,
                intervals: 32767
            }))
        );
        assert_eq!(
            request(&[L!("random"), L!("--"), L!("-1")]),
            Ok(Request::Seed(u64::MAX))
        );
        assert_eq!(request(&[L!("random"), L!(" +42 ")]), Ok(Request::Seed(42)));
        assert_eq!(
            request(&[L!("random"), L!("choice"), L!("-h")]),
            Ok(Request::Draw(Draw::Choice { first: 2, count: 1 }))
        );
        assert_eq!(
            request(&[L!("random"), L!("--"), L!("choice"), L!("")]),
            Ok(Request::Draw(Draw::Choice { first: 3, count: 1 }))
        );
        assert_eq!(
            request(&[L!("random"), L!("-h"), L!("ignored")]),
            Ok(Request::Help)
        );
        assert_eq!(
            request(&[L!("random"), L!("10"), L!("3"), L!("0")]),
            Ok(Request::Draw(Draw::Number {
                start: 0,
                step: 3,
                intervals: 3
            }))
        );
    }

    #[test]
    fn invalid_requests_preserve_error_kind_and_status() {
        for (args, expected) in [
            (vec![L!("random"), L!("choice")], ParseError::NoChoices),
            (
                vec![L!("random"), L!("-1")],
                ParseError::UnknownOption(L!("-1")),
            ),
            (
                vec![L!("random"), L!("--help=yes")],
                ParseError::UnexpectedOptionArgument(L!("--help=yes")),
            ),
            (
                vec![L!("random"), L!("0"), L!("0"), L!("10")],
                ParseError::ZeroStep,
            ),
            (
                vec![L!("random"), L!("0"), L!("-1"), L!("10")],
                ParseError::InvalidInteger(L!("-1")),
            ),
            (
                vec![L!("random"), L!("9223372036854775808")],
                ParseError::InvalidInteger(L!("9223372036854775808")),
            ),
            (
                vec![L!("random"), L!("0"), L!("18446744073709551616"), L!("10")],
                ParseError::InvalidInteger(L!("18446744073709551616")),
            ),
        ] {
            assert_eq!(request(&args), Err(expected));
            assert_eq!(expected.status(), 2);
        }
        assert_eq!(
            request(&[L!("random"), L!("0"), L!("1"), L!("2"), L!("3")]),
            Err(ParseError::TooManyArguments)
        );
        assert_eq!(ParseError::TooManyArguments.status(), 1);
    }

    #[test]
    fn ranges_include_full_width_endpoints_without_overflow() {
        let mut engine = seeded(42);
        for (start, step, end) in [
            (i64::MIN, 1, i64::MAX),
            (i64::MIN, u64::MAX, i64::MAX),
            (-10, 3, 10),
            (5, u64::MAX, 5),
        ] {
            let args: Vec<WString> = [
                "random".to_owned(),
                "--".to_owned(),
                start.to_string(),
                step.to_string(),
                end.to_string(),
            ]
            .into_iter()
            .map(WString::from)
            .collect();
            let Request::Draw(draw) =
                parse(&mut args.iter().map(WString::as_utfstr).collect::<Vec<_>>()).unwrap()
            else {
                panic!("expected a range");
            };
            for _ in 0..256 {
                let Value::Number(value) = draw.sample(&mut engine) else {
                    panic!("expected a number")
                };
                assert!((start..=end).contains(&value));
                assert_eq!(value.abs_diff(start) % step, 0);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn serialized_engine_continues_the_same_sequence() {
        let mut engine = seeded(42);
        for _ in 0..37 {
            engine.next_u64();
        }
        let mut restored: Engine =
            bincode::deserialize(&bincode::serialize(&engine).unwrap()).unwrap();
        for _ in 0..256 {
            assert_eq!(engine.next_u64(), restored.next_u64());
        }
    }
}
