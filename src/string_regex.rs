//! Shared UTF-32 string matching and replacement, independent of shell I/O and variables.
use fish_feature_flags::{FeatureFlag, feature_test};
use fish_widestring::{prelude::*, str2wcstring};
use pcre2::utf32::{Captures, Regex, RegexBuilder};
use std::{borrow::Cow, collections::HashMap};

#[derive(Clone, Copy, Default)]
pub(crate) struct MatchOptions {
    pub all: bool,
    pub entire: bool,
    pub groups_only: bool,
    pub ignore_case: bool,
    pub invert_match: bool,
    pub quiet: bool,
    pub index: bool,
    pub regex: bool,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct ReplaceOptions {
    pub all: bool,
    pub ignore_case: bool,
    pub regex: bool,
}

pub(crate) struct RegexMatcher {
    regex: Regex,
    pub total_matched: usize,
    pub first_match_captures: HashMap<String, Vec<WString>>,
    opts: MatchOptions,
}

enum MatchResult<'a> {
    NoMatch,
    Match(Option<Captures<'a>>),
}

#[derive(Debug)]
pub(crate) enum RegexError {
    Compile {
        pattern: WString,
        error: pcre2::Error,
    },
    InvalidCaptureGroupName {
        name: WString,
    },
    InvalidEscape {
        replacement: WString,
    },
}

impl RegexMatcher {
    pub(crate) fn new(
        pattern: &wstr,
        opts: MatchOptions,
        is_read_only: impl Fn(&wstr) -> bool,
    ) -> Result<Self, RegexError> {
        let regex = RegexBuilder::new()
            .caseless(opts.ignore_case)
            // UTF-mode can be enabled with `(*UTF)` https://www.pcre.org/current/doc/html/pcre2unicode.html
            // we use the capture group names to set local variables, and those are limited
            // to ascii-alphanumerics and underscores in non-UTF-mode
            // https://www.pcre.org/current/doc/html/pcre2syntax.html#SEC13
            // we can probably relax this limitation as long as we ensure
            // the capture group names are valid variable names
            .block_utf_pattern_directive(true)
            .build(pattern.as_char_slice())
            .map_err(|error| RegexError::Compile {
                pattern: pattern.to_owned(),
                error,
            })?;

        Self::validate_capture_group_names(regex.capture_names(), is_read_only)?;

        let first_match_captures = regex
            .capture_names()
            .iter()
            .filter_map(|name| name.as_ref().map(|n| (n.to_owned(), Vec::new())))
            .collect();
        let m = Self {
            regex,
            total_matched: 0,
            first_match_captures,
            opts,
        };
        Ok(m)
    }

    pub(crate) fn report_matches(
        &mut self,
        arg: &wstr,
        mut emit: impl FnMut(&wstr),
    ) -> Result<(), pcre2::Error> {
        let mut iter = self.regex.captures_iter(arg.as_char_slice());
        let cg = iter.next().transpose()?;
        let rc = self.report_match(arg, cg, &mut emit);

        let mut populate_captures = false;
        if let MatchResult::Match(actual) = &rc {
            populate_captures = self.total_matched == 0;
            self.total_matched += 1;

            if populate_captures {
                Self::populate_captures_from_match(
                    &mut self.first_match_captures,
                    &self.opts,
                    actual.as_ref(),
                );
            }
        }

        if !self.opts.invert_match && self.opts.all {
            // we are guaranteed to match as long as ops.invert_match is false
            while let MatchResult::Match(cg) =
                self.report_match(arg, iter.next().transpose()?, &mut emit)
            {
                if populate_captures {
                    Self::populate_captures_from_match(
                        &mut self.first_match_captures,
                        &self.opts,
                        cg.as_ref(),
                    );
                }
            }
        }
        Ok(())
    }

    fn populate_captures_from_match<'a>(
        first_match_captures: &mut HashMap<String, Vec<WString>>,
        opts: &MatchOptions,
        cg: Option<&Captures<'a>>,
    ) {
        for (name, captures) in first_match_captures.iter_mut() {
            // If there are multiple named groups and --all was used, we need to ensure that
            // the indexes are always in sync between the variables. If an optional named
            // group didn't match but its brethren did, we need to make sure to put
            // *something* in the resulting array, and unfortunately fish doesn't support
            // empty/null members so we're going to have to use an empty string as the
            // sentinel value.

            if let Some(m) = cg.as_ref().and_then(|cg| cg.name(&name.clone())) {
                captures.push(WString::from(m.as_bytes()));
            } else if opts.all {
                captures.push(WString::new());
            }
        }
    }

    fn validate_capture_group_names(
        capture_group_names: &[Option<String>],
        is_read_only: impl Fn(&wstr) -> bool,
    ) -> Result<(), RegexError> {
        for name in capture_group_names.iter().filter_map(|n| n.as_ref()) {
            let wname = str2wcstring(name);
            if is_read_only(&wname) {
                return Err(RegexError::InvalidCaptureGroupName { name: wname });
            }
        }
        Ok(())
    }

    fn report_match<'a>(
        &self,
        arg: &'a wstr,
        cg: Option<Captures<'a>>,
        emit: &mut impl FnMut(&wstr),
    ) -> MatchResult<'a> {
        let Some(cg) = cg else {
            if self.opts.invert_match && !self.opts.quiet {
                if self.opts.index {
                    emit(&WString::from(format!("1 {}", arg.len())));
                } else {
                    emit(arg);
                }
            }
            return match self.opts.invert_match {
                true => MatchResult::Match(None),
                false => MatchResult::NoMatch,
            };
        };

        if self.opts.invert_match {
            return MatchResult::NoMatch;
        }

        if self.opts.quiet {
            return MatchResult::Match(Some(cg));
        }

        if self.opts.entire {
            emit(arg);
        }

        let start = (self.opts.entire || self.opts.groups_only) as usize;

        for m in (start..cg.len()).filter_map(|i| cg.get(i)) {
            if self.opts.index {
                emit(&WString::from(format!(
                    "{} {}",
                    m.start() + 1,
                    m.end() - m.start()
                )));
            } else {
                emit(&arg[m.start()..m.end()]);
            }
        }

        MatchResult::Match(Some(cg))
    }
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum StringReplacer<'args> {
    Regex {
        replacement: WString,
        regex: Regex,
        opts: ReplaceOptions,
    },
    Literal {
        pattern: Cow<'args, wstr>,
        replacement: Cow<'args, wstr>,
        opts: ReplaceOptions,
    },
}

impl<'args> StringReplacer<'args> {
    fn interpret_escape(arg: &'args wstr) -> Option<WString> {
        use fish_wcstringutil::escape::unescape_one;

        let mut result: WString = WString::with_capacity(arg.len());
        let mut cursor = arg;
        while !cursor.is_empty() {
            if cursor.char_at(0) == '\\' {
                let escape_len = unescape_one(cursor, &mut result, true, false)?;
                cursor = cursor.slice_from(escape_len);
            } else {
                result.push(cursor.char_at(0));
                cursor = cursor.slice_from(1);
            }
        }
        Some(result)
    }

    pub(crate) fn new(
        pattern: &'args wstr,
        replacement: &'args wstr,
        opts: ReplaceOptions,
    ) -> Result<Self, RegexError> {
        let r = match (opts.regex, opts.ignore_case) {
            (true, _) => {
                let regex = RegexBuilder::new()
                    .caseless(opts.ignore_case)
                    // set to behave similarly to match, could probably be either enabled by default or
                    // allowed to be user-controlled here
                    .block_utf_pattern_directive(true)
                    .build(pattern.as_char_slice())
                    .map_err(|error| RegexError::Compile {
                        pattern: pattern.to_owned(),
                        error,
                    })?;

                let replacement = if feature_test(FeatureFlag::StringReplaceBackslash) {
                    replacement.to_owned()
                } else {
                    Self::interpret_escape(replacement).ok_or_else(|| {
                        RegexError::InvalidEscape {
                            replacement: replacement.to_owned(),
                        }
                    })?
                };
                Self::Regex {
                    replacement,
                    regex,
                    opts,
                }
            }
            (false, true) => Self::Literal {
                // previously we used wcsncasecmp but there is no equivalent function in Rust widestring
                // this should likely be handled by a using the `literal` option on our regex
                pattern: Cow::Owned(pattern.to_lowercase()),
                replacement: Cow::Owned(replacement.to_owned()),
                opts,
            },
            (false, false) => Self::Literal {
                pattern: Cow::Borrowed(pattern),
                replacement: Cow::Borrowed(replacement),
                opts,
            },
        };

        Ok(r)
    }

    pub(crate) fn replace<'a>(
        &self,
        arg: Cow<'a, wstr>,
    ) -> Result<(bool, Cow<'a, wstr>), pcre2::Error> {
        match self {
            StringReplacer::Regex {
                replacement,
                regex,
                opts,
            } => {
                let res = if opts.all {
                    regex.replace_all(arg.as_char_slice(), replacement.as_char_slice(), true)
                } else {
                    regex.replace(arg.as_char_slice(), replacement.as_char_slice(), true)
                }?;

                let res = match res {
                    Cow::Borrowed(_slice_of_arg) => (false, arg),
                    Cow::Owned(s) => (true, Cow::Owned(WString::from_chars(s))),
                };
                Ok(res)
            }
            StringReplacer::Literal {
                pattern,
                replacement,
                opts,
            } => {
                if pattern.is_empty() {
                    return Ok((false, arg));
                }

                // a premature optimization would be to alloc larger if we have replacement.len() > pattern.len()
                let mut result = WString::with_capacity(arg.len());

                let subject = if opts.ignore_case {
                    arg.to_lowercase()
                } else {
                    arg.as_ref().to_owned()
                };

                let mut offset = 0;
                while let Some(idx) = subject[offset..].find(pattern.as_char_slice()) {
                    result.push_utfstr(&subject[offset..offset + idx]);
                    result.push_utfstr(&replacement);
                    offset += idx + pattern.len();
                    if !opts.all {
                        break;
                    }
                }
                if offset == 0 {
                    return Ok((false, arg));
                }
                result.push_utfstr(&arg[offset..]);

                Ok((true, Cow::Owned(result)))
            }
        }
    }
}

pub(crate) struct WildCardMatcher {
    pattern: WString,
    total_matched: usize,
    opts: MatchOptions,
}

impl WildCardMatcher {
    fn new(pattern: &wstr, opts: MatchOptions) -> Self {
        use fish_widestring::ANY_STRING;
        let mut pattern = fish_wcstringutil::unescape_wildcards(
            pattern,
            !feature_test(FeatureFlag::QuestionMarkNoGlob),
        );
        if opts.ignore_case {
            pattern = pattern.to_lowercase();
        }
        if opts.entire {
            if !pattern.is_empty() {
                if pattern.char_at(0) != ANY_STRING {
                    pattern.insert(0, ANY_STRING);
                }
                if pattern.char_at(pattern.len() - 1) != ANY_STRING {
                    pattern.push(ANY_STRING);
                }
            } else {
                pattern.push(ANY_STRING);
            }
        }
        Self {
            pattern,
            total_matched: 0,
            opts,
        }
    }

    fn report_matches(&mut self, arg: &wstr, mut emit: impl FnMut(&wstr)) {
        // --all is a no-op for glob matching: the pattern matches the entire argument.
        let subject = if self.opts.ignore_case {
            arg.to_lowercase()
        } else {
            arg.to_owned()
        };
        if fish_wcstringutil::wildcard_match(subject, &self.pattern, false) ^ self.opts.invert_match
        {
            self.total_matched += 1;
            if !self.opts.quiet {
                if self.opts.index {
                    emit(&WString::from(format!("1 {}", arg.len())));
                } else {
                    emit(arg);
                }
            }
        }
    }
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum StringMatcher {
    Regex(RegexMatcher),
    WildCard(WildCardMatcher),
}

impl StringMatcher {
    pub(crate) fn new(
        pattern: &wstr,
        opts: MatchOptions,
        is_read_only: impl Fn(&wstr) -> bool,
    ) -> Result<Self, RegexError> {
        if opts.regex {
            Ok(Self::Regex(RegexMatcher::new(pattern, opts, is_read_only)?))
        } else {
            Ok(Self::WildCard(WildCardMatcher::new(pattern, opts)))
        }
    }

    pub(crate) fn report_matches(
        &mut self,
        arg: &wstr,
        emit: impl FnMut(&wstr),
    ) -> Result<(), pcre2::Error> {
        match self {
            Self::Regex(m) => m.report_matches(arg, emit)?,
            Self::WildCard(m) => m.report_matches(arg, emit),
        }
        Ok(())
    }

    pub(crate) fn match_count(&self) -> usize {
        match self {
            Self::Regex(m) => m.total_matched,
            Self::WildCard(m) => m.total_matched,
        }
    }

    pub(crate) fn named_captures(self) -> HashMap<String, Vec<WString>> {
        match self {
            Self::Regex(m) => m.first_match_captures,
            Self::WildCard(_) => HashMap::new(),
        }
    }
}
