//! Feasibility prototype for Bash completion contexts. This is not an evaluator.

use crate::complete::{
    CompleteFlags, Completion, CompletionRequestOptions, CompletionView, WantsEscaping,
};
use crate::env::Environment;
use crate::prelude::*;
use std::ops::Range;

#[derive(Debug, PartialEq)]
pub enum Unsupported {
    Syntax,
    Expansion,
    Cursor,
    Replacement,
}

#[derive(Clone, Debug)]
pub struct Word {
    pub source: Range<usize>,
    pub projected: Range<usize>,
    // Expansions are indivisible: their current value must never replace their source.
    pub has_expansion: bool,
}

#[derive(Debug)]
pub struct Projection {
    pub text: WString,
    pub cursor: usize,
    pub words: Vec<Word>,
    pub boundaries: Vec<Option<usize>>,
    source: WString,
    source_cursor: usize,
    quote: Option<char>,
    scalars: Vec<WString>,
    fish_evaluation: bool,
}

fn name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn name_char(c: char) -> bool {
    name_start(c) || c.is_ascii_digit()
}

impl Projection {
    pub fn validate_state(&self, state: &crate::bash_session::State) -> Result<(), Unsupported> {
        if state.fish_evaluation {
            return Ok(());
        }
        if self
            .scalars
            .iter()
            .any(|name| !state.scalars.contains_key(name))
        {
            return Err(Unsupported::Expansion);
        }
        Ok(())
    }

    pub fn complete(
        &self,
        options: CompletionRequestOptions,
        ctx: &mut crate::operation_context::OperationContext,
    ) -> Result<(Vec<Completion>, Vec<WString>), Unsupported> {
        if let Some(state) = ctx.bash_state {
            self.validate_state(state)?;
            // shortcut: alias argument projection needs recursive expansion provenance.
            // Refuse it until aliases can be expanded without losing edit ownership.
            let syntax = crate::bash_syntax::Syntax::for_input(&self.source, state.fish_evaluation);
            let range = syntax.process_extent(self.source_cursor, false);
            let words = self
                .words
                .iter()
                .filter(|w| w.source.start >= range.start)
                .collect::<Vec<_>>();
            if let Some(word) = words.first() {
                if self.source_cursor > word.source.end {
                    let name = crate::bash_syntax::unquote(&self.source[word.source.clone()]);
                    if name.is_some_and(|n| state.aliases.contains_key(&n)) {
                        return Err(Unsupported::Expansion);
                    }
                }
            }
        }
        let rejected_edit = std::rc::Rc::new(std::cell::Cell::new(false));
        let _view = ctx.maybe_parser().map(|parser| {
            parser
                .libdata()
                .completion_view
                .scoped_replace(Some(CompletionView {
                    text: self.text.clone(),
                    cursor: self.cursor,
                    rejected_edit: rejected_edit.clone(),
                }))
        });
        let mut result = crate::complete::complete(&self.text[..self.cursor], options, ctx);
        if result.0.is_empty() && matches!(options, CompletionRequestOptions::Autosuggestion) {
            let (candidates, loads) =
                crate::complete::complete_option_as_autosuggestion(&self.text[..self.cursor], ctx);
            result.0 = candidates;
            result.1.extend(loads);
        }
        if rejected_edit.get() {
            Err(Unsupported::Replacement)
        } else {
            Ok(result)
        }
    }
    /// Project a deliberately limited, non-executing subset. The entire source is checked,
    /// including text after the cursor, before any completion provider is invoked.
    pub fn new(source: &wstr, cursor: usize, vars: &dyn Environment) -> Result<Self, Unsupported> {
        Self::project(source, cursor, vars, false)
    }

    pub fn for_state(
        source: &wstr,
        cursor: usize,
        vars: &dyn Environment,
        state: &crate::bash_session::State,
    ) -> Result<Self, Unsupported> {
        Self::project(source, cursor, vars, state.fish_evaluation)
    }

    fn project(
        source: &wstr,
        cursor: usize,
        vars: &dyn Environment,
        fish: bool,
    ) -> Result<Self, Unsupported> {
        if cursor > source.len() {
            return Err(Unsupported::Cursor);
        }
        let mut p = Self {
            text: WString::new(),
            cursor: 0,
            words: vec![],
            boundaries: vec![Some(0)],
            source: source.to_owned(),
            source_cursor: cursor,
            quote: None,
            scalars: vec![],
            fish_evaluation: fish,
        };
        let s = source.as_char_slice();
        let mut i = 0;
        let mut quote = None;
        let mut word: Option<Word> = None;
        let mut command_start = true;
        while i < s.len() {
            if i == cursor {
                p.cursor = p.text.len();
                p.quote = quote;
            }
            let c = s[i];
            if quote.is_none() && (c.is_whitespace() || matches!(c, ';' | '|' | '&')) {
                if let Some(w) = word.take() {
                    p.words.push(w);
                }
                if !c.is_whitespace() || c == '\n' {
                    command_start = true;
                }
                p.copy(c, i);
                i += 1;
                continue;
            }
            if quote.is_none() && c == '#' && word.is_none() {
                while i < s.len() && s[i] != '\n' {
                    if i == cursor {
                        return Err(Unsupported::Cursor);
                    }
                    p.copy(s[i], i);
                    i += 1;
                }
                continue;
            }
            if word.is_none() {
                if command_start {
                    let end = s[i..]
                        .iter()
                        .position(|c| c.is_whitespace() || ";|&".contains(*c))
                        .map_or(s.len(), |n| i + n);
                    // A simple-command view is not adequate for compound syntax or assignments.
                    if matches!(
                        source[i..end].to_string().as_str(),
                        "if" | "then"
                            | "elif"
                            | "else"
                            | "fi"
                            | "for"
                            | "while"
                            | "until"
                            | "do"
                            | "done"
                            | "case"
                            | "esac"
                            | "function"
                            | "select"
                            | "[["
                            | "]]"
                            | "!"
                            | "time"
                            | "coproc"
                            | "and"
                            | "or"
                            | "not"
                            | "begin"
                            | "end"
                            | "switch"
                    ) || (!fish
                        && matches!(
                            source[i..end].to_string().as_str(),
                            "builtin" | "set" | "read" | "source" | "eval"
                        ))
                        || source[i..end].contains('=')
                    {
                        return Err(Unsupported::Syntax);
                    }
                    command_start = false;
                }
                word = Some(Word {
                    source: i..i,
                    projected: p.text.len()..p.text.len(),
                    has_expansion: false,
                });
            }
            match (quote, c) {
                (Some('\''), '\'') | (Some('"'), '"') => {
                    quote = None;
                    p.copy(c, i);
                    i += 1;
                }
                (None, '\'' | '"') => {
                    quote = Some(c);
                    p.copy(c, i);
                    i += 1;
                }
                (Some('\''), '\\') => {
                    // Fish, unlike Bash, recognizes \\ and \' within single quotes.
                    p.text.push('\\');
                    p.boundaries.push(None);
                    p.copy(c, i);
                    i += 1;
                }
                (Some('\''), _) => {
                    p.copy(c, i);
                    i += 1;
                }
                (_, '\\') => {
                    let Some(&next) = s.get(i + 1) else {
                        return Err(Unsupported::Syntax);
                    };
                    if cursor == i + 1 {
                        return Err(Unsupported::Cursor);
                    }
                    if quote.is_none() && next != '\n' {
                        // Bash quotes the next character; Fish also recognizes escapes such as \t.
                        p.text.push('\'');
                        p.boundaries.push(None);
                        if matches!(next, '\\' | '\'') {
                            p.text.push('\\');
                            p.boundaries.push(None);
                        }
                        p.text.push(next);
                        p.boundaries.push(None);
                        p.text.push('\'');
                        p.boundaries.push(Some(i + 2));
                        i += 2;
                        let w = word.as_mut().unwrap();
                        w.source.end = i;
                        w.projected.end = p.text.len();
                        continue;
                    }
                    if !fish && quote == Some('"') && next == '`' {
                        // Bash removes this escape; Fish treats a backtick as ordinary text.
                        p.copy(next, i + 1);
                        i += 2;
                        let w = word.as_mut().unwrap();
                        w.source.end = i;
                        w.projected.end = p.text.len();
                        continue;
                    }
                    // In double quotes only these characters lose their backslash.
                    if quote == Some('"') && !matches!(next, '$' | '`' | '"' | '\\' | '\n') {
                        p.text.push('\\');
                        p.boundaries.push(None);
                    }
                    p.copy(c, i);
                    p.copy(next, i + 1);
                    i += 2;
                }
                (_, '$') => {
                    let braced = s.get(i + 1) == Some(&'{');
                    let start = i + if braced { 2 } else { 1 };
                    if !s.get(start).is_some_and(|c| name_start(*c)) {
                        return Err(Unsupported::Expansion);
                    }
                    let mut end = start + 1;
                    while s.get(end).is_some_and(|c| name_char(*c)) {
                        end += 1;
                    }
                    if braced && s.get(end) != Some(&'}') {
                        return Err(Unsupported::Expansion);
                    }
                    let source_end = end + usize::from(braced);
                    if (i + 1..source_end).contains(&cursor) {
                        return Err(Unsupported::Cursor);
                    }
                    // Only ordinary scalars are accepted. The caller supplies a Bash snapshot,
                    // not Fish lists, arrays, special parameters, or live expansion requests.
                    p.scalars.push(source[start..end].to_owned());
                    if !fish {
                        let value = vars
                            .get(&source[start..end])
                            .ok_or(Unsupported::Expansion)?;
                        if value.as_list().len() != 1 || quote.is_none() {
                            return Err(Unsupported::Expansion);
                        }
                    }
                    // Braces cannot simply be removed before an adjacent identifier.
                    if s.get(source_end) == Some(&'[')
                        || (braced && s.get(source_end).is_some_and(|c| name_char(*c)))
                    {
                        return Err(Unsupported::Expansion);
                    }
                    p.text.push('$');
                    p.boundaries.push(None);
                    for c in source[start..end].chars() {
                        p.text.push(c);
                        p.boundaries.push(None);
                    }
                    *p.boundaries.last_mut().unwrap() = Some(source_end);
                    word.as_mut().unwrap().has_expansion = true;
                    i = source_end;
                }
                (None, '~')
                    if fish
                        && word.as_ref().unwrap().source.start == i
                        && s.get(i + 1) == Some(&'/') =>
                {
                    // Keep the expression for Fish to expand; never replace it with today's HOME.
                    word.as_mut().unwrap().has_expansion = true;
                    p.copy(c, i);
                    i += 1;
                }
                (_, '`') if !fish => return Err(Unsupported::Expansion),
                (None, '(' | ')' | '{' | '}' | '<' | '>' | '*' | '?' | '[' | '~') => {
                    return Err(Unsupported::Expansion);
                }
                _ => {
                    p.copy(c, i);
                    i += 1;
                }
            }
            let w = word.as_mut().unwrap();
            w.source.end = i;
            w.projected.end = p.text.len();
        }
        if let Some(w) = word {
            p.words.push(w);
        }
        if cursor == s.len() {
            p.cursor = p.text.len();
            p.quote = quote;
        }
        Ok(p)
    }

    fn copy(&mut self, c: char, source: usize) {
        self.text.push(c);
        self.boundaries.push(Some(source + 1));
    }

    pub fn current_word(&self) -> Option<&Word> {
        self.words
            .iter()
            .find(|w| w.source.start <= self.source_cursor && self.source_cursor <= w.source.end)
    }

    /// Apply only the requested candidate; never reconstruct the rest of the Bash buffer.
    pub fn apply(
        &self,
        candidate: &Completion,
        append_only: bool,
    ) -> Result<(WString, usize), Unsupported> {
        self.apply_text(&candidate.completion, candidate.flags, append_only)
    }

    pub fn apply_text(
        &self,
        value: &wstr,
        flags: CompleteFlags,
        append_only: bool,
    ) -> Result<(WString, usize), Unsupported> {
        // Raw Fish edits, variable-name suffixes and line replacements need a separate contract.
        if matches!(flags.wants_escaping, WantsEscaping::No)
            || flags.replaces_line()
            || flags.suffix_type().is_some_and(|s| s.for_variable_name)
            || flags.keep_variable_override_prefix
        {
            return Err(Unsupported::Replacement);
        }
        let word = self.current_word();
        let mut range = self.source_cursor..self.source_cursor;
        let mut quote = self.quote;
        if flags.replaces_token() {
            if append_only || word.is_some_and(|w| w.has_expansion) {
                return Err(Unsupported::Replacement);
            }
            range = word.map_or(range, |w| w.source.clone());
            quote = None;
        } else if !append_only && quote.is_none() && self.source_cursor > 0 {
            let trailing = self.source.char_at(self.source_cursor - 1);
            if matches!(trailing, '\'' | '"') && word.is_some() {
                // Only re-enter a syntactic closing quote, never an escaped literal quote.
                let n = self.source[..self.source_cursor - 1]
                    .chars()
                    .rev()
                    .take_while(|c| *c == '\\')
                    .count();
                if n % 2 == 0 {
                    range.start -= 1;
                    range.end -= 1;
                    quote = Some(trailing);
                }
            }
        }
        let mut insert = WString::new();
        for c in value.chars() {
            match quote {
                Some('\'') if c == '\'' => insert.push_utfstr(L!("'\\''")),
                Some('\'') => insert.push(c),
                Some('"') => {
                    if matches!(c, '$' | '"' | '\\' | '!') || (!self.fish_evaluation && c == '`') {
                        // A literal ! cannot be portably escaped within Bash double quotes.
                        if c == '!' {
                            return Err(Unsupported::Replacement);
                        }
                        insert.push('\\');
                    }
                    insert.push(c);
                }
                None if c == '\n' => insert.push_utfstr(L!("'\n'")),
                None => {
                    if !c.is_alphanumeric() && !"/_-.,:=@%+".contains(c) {
                        insert.push('\\');
                    }
                    insert.push(c);
                }
                _ => unreachable!(),
            }
        }
        let mut cursor = range.start + insert.len();
        if flags.suffix_type().is_some() {
            if let Some(q) = quote {
                if self.source.char_at(range.end) == q {
                    range.end += 1;
                }
                insert.push(q);
                cursor += 1;
            }
            if self.source.char_at(range.end) != ' ' {
                insert.push(' ');
            } else {
                range.end += 1;
                insert.push(' ');
            }
            cursor += 1;
        }
        Ok((
            self.source[..range.start].to_owned() + insert.as_utfstr() + &self.source[range.end..],
            cursor,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::WantsSuffix;
    use crate::tests::prelude::TestEnvironment;

    #[test]
    fn projection_preserves_context() {
        let mut vars = TestEnvironment::default();
        vars.vars
            .insert(L!("repo").to_owned(), L!("/tmp/a b").to_owned());
        let text = L!("echo ok |\ngit -C \"${repo}\" switch  ");
        let p = Projection::new(text, text.len(), &vars).unwrap();
        assert_eq!(p.text, L!("echo ok |\ngit -C \"$repo\" switch  "));
        assert_eq!(p.cursor, p.text.len());
        assert_eq!(p.boundaries[p.cursor], Some(text.len()));
        assert!(p.words.iter().any(|w| w.has_expansion));
        let candidate = Completion::from_completion(L!("topic").to_owned());
        let (result, _) = p.apply(&candidate, false).unwrap();
        assert_eq!(result, text.to_owned() + L!("topic "));
    }

    #[test]
    fn projection_never_runs_expansions() {
        let vars = TestEnvironment::default();
        for text in [
            L!("git -C $(touch sentinel) switch "),
            L!("echo ${x:=changed} "),
            L!("echo $((x++)) "),
            L!("echo `touch sentinel` "),
            L!("echo <(touch sentinel) "),
            L!("echo ${a[@]} "),
            L!("echo $missing "),
            L!("cat <<EOF\n"),
        ] {
            assert!(Projection::new(text, text.len(), &vars).is_err(), "{text}");
        }
    }

    #[test]
    fn fish_expansion_context_and_bash_lexical_escapes() {
        let vars = TestEnvironment::default();
        let state = crate::bash_session::State {
            fish_evaluation: true,
            ..Default::default()
        };
        let source = L!("printf '%s' $missing \\t\\ü");
        let projection = Projection::for_state(source, source.len(), &vars, &state).unwrap();
        assert_eq!(projection.text, L!("printf '%s' $missing 't''ü'"));
        let (edited, _) = projection
            .apply(&Completion::from_completion(L!("opic").to_owned()), false)
            .unwrap();
        assert_eq!(edited, source.to_owned() + L!("opic "));
        assert!(Projection::new(source, source.len(), &vars).is_err());
    }

    #[test]
    fn fish_home_prefix_preserves_expansion_and_source() {
        let vars = TestEnvironment::default();
        let state = crate::bash_session::State {
            fish_evaluation: true,
            ..Default::default()
        };
        let source = L!("git -C ~/repo switch topic-  ");
        let p = Projection::for_state(source, source.len(), &vars, &state).unwrap();
        assert_eq!(p.text, source);
        assert_eq!(p.cursor, source.len());
        assert_eq!(
            p.boundaries,
            (0..=source.len()).map(Some).collect::<Vec<_>>()
        );
        assert!(p.words[2].has_expansion);
        assert!(Projection::new(source, source.len(), &vars).is_err());

        let source = L!("cat ~/fi tail");
        let cursor = L!("cat ~/fi").len();
        let p = Projection::for_state(source, cursor, &vars, &state).unwrap();
        let mut c = Completion::from_completion(L!("le ü;$`").to_owned());
        assert_eq!(
            p.apply(&c, false).unwrap(),
            (
                L!("cat ~/file\\ ü\\;\\$\\` tail").to_owned(),
                L!("cat ~/file\\ ü\\;\\$\\` ").len()
            )
        );
        c.flags.replaces = Some(crate::complete::ReplacementScope::Token(Default::default()));
        assert_eq!(p.apply(&c, false), Err(Unsupported::Replacement));

        for source in [L!("cat '~/fi"), L!("cat \"~/fi"), L!("cat \\~/fi")] {
            let p = Projection::for_state(source, source.len(), &vars, &state).unwrap();
            assert!(!p.current_word().unwrap().has_expansion);
        }
        for source in [
            L!("cat ~"),
            L!("cat ~user/"),
            L!("cat ~+/"),
            L!("cat path~/"),
        ] {
            assert!(Projection::for_state(source, source.len(), &vars, &state).is_err());
        }
    }

    #[test]
    fn fish_backticks_are_literal_in_projection_and_insertion() {
        let vars = TestEnvironment::default();
        let state = crate::bash_session::State {
            fish_evaluation: true,
            ..Default::default()
        };
        for source in [L!("cat `fi"), L!("cat \"`fi"), L!("cat \"\\`fi")] {
            let p = Projection::for_state(source, source.len(), &vars, &state).unwrap();
            assert_eq!(p.text, source);
            assert_eq!(p.cursor, source.len());
        }
        let source = L!("cat \"`fi");
        let p = Projection::for_state(source, source.len(), &vars, &state).unwrap();
        assert_eq!(
            p.apply(&Completion::from_completion(L!("le`").to_owned()), false)
                .unwrap()
                .0,
            L!("cat \"`file`\" ")
        );
        assert!(Projection::new(source, source.len(), &vars).is_err());
    }

    #[test]
    fn insertion_preserves_source_and_quotes() {
        let vars = TestEnvironment::default();
        for (text, cursor, value, expected) in [
            (
                L!("echo fi | cat"),
                7,
                L!("le;$()` ü"),
                L!("echo file\\;\\$\\(\\)\\`\\ ü | cat"),
            ),
            (L!("echo 'fi'"), 9, L!("le's"), L!("echo 'file'\\''s' ")),
            (
                L!("echo \"fi"),
                8,
                L!("le$`\\"),
                L!("echo \"file\\$\\`\\\\\" "),
            ),
            (L!("echo --file=fi"), 14, L!("le"), L!("echo --file=file ")),
        ] {
            let p = Projection::new(text, cursor, &vars).unwrap();
            let (actual, _) = p
                .apply(&Completion::from_completion(value.to_owned()), false)
                .unwrap();
            assert_eq!(actual, expected);
        }
        let p = Projection::new(L!("cd fo"), 5, &vars).unwrap();
        let mut c = Completion::from_completion(L!("o/").to_owned());
        c.flags.wants_suffix = WantsSuffix::No;
        assert_eq!(p.apply(&c, false).unwrap().0, L!("cd foo/"));
    }

    #[test]
    fn inserted_literals_match_bash() {
        use std::process::Command;
        let vars = TestEnvironment::default();
        for prefix in [
            L!("printf '%s' fi"),
            L!("printf '%s' 'fi"),
            L!("printf '%s' \"fi"),
        ] {
            for suffix in [
                "le",
                "le;$(false)`false`",
                "le's\\name",
                "le ü🐟",
                "le\nnext",
                "le[1]*?",
            ] {
                let p = Projection::new(prefix, prefix.len(), &vars).unwrap();
                let c = Completion::from_completion(WString::from(suffix));
                let (text, _) = p.apply(&c, false).unwrap();
                let result = Command::new("/bin/bash")
                    .args(["--noprofile", "--norc", "-c", &text.to_string()])
                    .env_remove("BASH_ENV")
                    .output()
                    .unwrap();
                assert!(result.status.success(), "{text}: {:?}", result.stderr);
                assert_eq!(
                    String::from_utf8(result.stdout).unwrap(),
                    format!("fi{suffix}"),
                    "{text}"
                );
            }
        }
    }

    #[test]
    #[crate::tests::prelude::serial]
    fn bundled_completion_feasibility() {
        use crate::complete::{CompletionRequestOptions, complete, sort_and_prioritize};
        use crate::env::{EnvMode, EnvSetMode};
        use crate::io::IoChain;
        use crate::tests::prelude::{TestParser, test_init};
        use std::process::Command;

        test_init();
        let _ = crate::common::PROGRAM_NAME.set(L!("fish"));
        crate::signal::signal_set_handlers(false);
        let mut parser = TestParser::new();
        let root = env!("CARGO_MANIFEST_DIR");
        let globals = crate::env::EnvStack::globals();
        let saved = [
            L!("fish_function_path"),
            L!("fish_complete_path"),
            L!("PATH"),
            L!("IFS"),
            L!("HOME"),
        ]
        .map(|name| (name, globals.getf(name, EnvMode::GLOBAL)));
        let _restore = fish_common::ScopeGuard::new(saved, |saved| {
            for (name, value) in saved {
                match value {
                    Some(value) => {
                        let mode = EnvMode {
                            export: Some(value.exports()),
                            pathvar: Some(value.is_pathvar()),
                            ..EnvMode::GLOBAL
                        };
                        globals.set(name, EnvSetMode::new(mode, false), value.as_list().to_vec());
                    }
                    None => {
                        globals.remove(name, EnvSetMode::new(EnvMode::GLOBAL, false));
                    }
                }
            }
        });
        for (name, value) in [
            (L!("fish_function_path"), format!("{root}/share/functions")),
            (
                L!("fish_complete_path"),
                format!("{root}/share/completions"),
            ),
            (L!("PATH"), std::env::var("PATH").unwrap()),
            (L!("IFS"), "\n".to_owned()),
        ] {
            parser.vars().set_one(
                name,
                EnvSetMode::new(EnvMode::GLOBAL, false),
                WString::from(value.clone()),
            );
            crate::env::EnvStack::globals().set_one(
                name,
                EnvSetMode::new(EnvMode::GLOBAL, false),
                WString::from(value),
            );
        }
        let dir = fish_tempfile::new_dir().unwrap();
        parser.vars().set_one(
            L!("HOME"),
            EnvSetMode::new(EnvMode::GLOBAL, false),
            WString::from(dir.path().to_str().unwrap()),
        );
        let repo = dir.path().join("repo ü space");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "fixture",
                "--allow-empty",
            ],
            vec!["branch", "topic-one"],
            vec!["branch", "topic-two"],
        ] {
            let result = Command::new("git")
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(result.status.success(), "{:?}", result);
        }
        parser.vars().set_one(
            L!("repo"),
            EnvSetMode::new(EnvMode::GLOBAL, false),
            WString::from(repo.to_str().unwrap()),
        );
        // The actual bundled scripts and their dynamic Git subprocesses run unchanged.
        for (source, expected) in [
            (L!("git sw"), L!("itch")),
            (L!("git --ver"), L!("sion")),
            (L!("git -C \"${repo}\" switch topic-"), L!("one")),
            (L!("printf ok |\ngit -C \"$repo\" switch topic-"), L!("two")),
        ] {
            let p = Projection::new(source, source.len(), parser.vars()).unwrap();
            let options = CompletionRequestOptions::Completion;
            let (mut candidates, _) = p.complete(options, &mut parser.context()).unwrap();
            sort_and_prioritize(&mut candidates, options);
            assert!(
                candidates
                    .iter()
                    .any(|c| c.completion == expected && !c.description.is_empty()),
                "{source}: {candidates:?}"
            );
            for c in candidates {
                if let Ok((applied, _)) = p.apply(&c, false) {
                    if source.find(L!("${repo}")).is_some() {
                        assert!(applied.find(L!("${repo}")).is_some());
                    }
                }
            }
        }
        let state = crate::bash_session::State {
            fish_evaluation: true,
            ..Default::default()
        };
        let source = L!("git -C ~/repo\\ ü\\ space switch topic-");
        let p = Projection::for_state(source, source.len(), parser.vars(), &state).unwrap();
        let (candidates, _) = p
            .complete(CompletionRequestOptions::Completion, &mut parser.context())
            .unwrap();
        let c = candidates.iter().find(|c| c.completion == "one").unwrap();
        assert!(!c.description.is_empty());
        assert_eq!(p.apply(c, false).unwrap().0, source.to_owned() + L!("one "));

        std::fs::create_dir(dir.path().join("fixture-directory")).unwrap();
        std::fs::write(dir.path().join("fixture;$()` ü"), "").unwrap();
        for source in [L!("cat ~/fixture"), L!("cat ~/fixture tail")] {
            let cursor = L!("cat ~/fixture").len();
            let p = Projection::for_state(source, cursor, parser.vars(), &state).unwrap();
            let (candidates, _) = p
                .complete(CompletionRequestOptions::Completion, &mut parser.context())
                .unwrap();
            let c = candidates
                .iter()
                .find(|c| c.completion == ";$()` ü")
                .unwrap();
            let suffix = if source.ends_with(L!(" tail")) {
                L!("tail")
            } else {
                L!("")
            };
            assert_eq!(
                p.apply(c, false).unwrap().0,
                L!("cat ~/fixture\\;\\$\\(\\)\\`\\ ü ").to_owned() + suffix
            );
        }
        for options in [
            CompletionRequestOptions::Completion,
            CompletionRequestOptions::Autosuggestion,
        ] {
            let source = L!("cd ~/fixture-d");
            let vars = parser.vars().snapshot();
            let p = Projection::for_state(source, source.len(), &vars, &state).unwrap();
            let mut ctx = match options {
                CompletionRequestOptions::Completion => parser.context(),
                CompletionRequestOptions::Autosuggestion => {
                    crate::operation_context::OperationContext::background_interruptible(&vars)
                }
            };
            let (candidates, _) = p.complete(options, &mut ctx).unwrap();
            let c = candidates
                .iter()
                .find(|c| c.completion == "irectory/")
                .unwrap();
            assert_eq!(p.apply(c, false).unwrap().0, L!("cd ~/fixture-directory/"));
            assert_eq!(p.apply(c, true).unwrap().0, L!("cd ~/fixture-directory/"));
        }
        // A provider that looks at the raw quote sees the quote, not merely equivalent argv.
        // Define the raw-quote observer without shell-escaping a condition twice.
        parser.eval(L!("function projection_quote; string match -q '\"*' -- (commandline -ct); end; complete -c projection_probe -f -k -a 'zeta alpha' -d order -n projection_quote"), &IoChain::new());
        let source = L!("projection_probe \"");
        let p = Projection::new(source, source.len(), parser.vars()).unwrap();
        let (mut c, _) = complete(
            &p.text,
            CompletionRequestOptions::Completion,
            &mut parser.context(),
        );
        sort_and_prioritize(&mut c, CompletionRequestOptions::Completion);
        assert_eq!(
            c.iter()
                .map(|c| c.completion.to_string())
                .collect::<Vec<_>>(),
            ["zeta", "alpha"]
        );
        assert!(c.iter().all(|c| c.description == "order"));

        parser.eval(
            L!(r#"function projection_context
            test (commandline -C) = 20
            and string match -q '* tail' -- (commandline)
            and test (commandline -ct) = pre
        end
        complete -c projection_probe -f -a prefix -n projection_context
        "#),
            &IoChain::new(),
        );
        let text = L!("projection_probe pre tail");
        let cursor = L!("projection_probe pre").len();
        assert_eq!(cursor, 20);
        let p = Projection::new(text, cursor, parser.vars()).unwrap();
        let (candidates, _) = p
            .complete(CompletionRequestOptions::Completion, &mut parser.context())
            .unwrap();
        let c = candidates.iter().find(|c| c.completion == "fix").unwrap();
        assert_eq!(
            p.apply(c, false).unwrap(),
            (L!("projection_probe prefix tail").to_owned(), 24)
        );

        parser.eval(
            L!("complete -c projection_edit -f -a '(commandline -r erased; echo candidate)'"),
            &IoChain::new(),
        );
        let text = L!("projection_edit ");
        let p = Projection::new(text, text.len(), parser.vars()).unwrap();
        assert_eq!(
            p.complete(CompletionRequestOptions::Completion, &mut parser.context())
                .unwrap_err(),
            Unsupported::Replacement
        );
        assert_eq!(p.source, text);
        assert!(parser.libdata().completion_view.borrow().is_none());

        parser.eval(L!("complete -c projection_wrap -w git"), &IoChain::new());
        let text = L!("projection_wrap -C \"${repo}\" switch topic-");
        let p = Projection::new(text, text.len(), parser.vars()).unwrap();
        let (candidates, _) = p
            .complete(CompletionRequestOptions::Completion, &mut parser.context())
            .unwrap();
        assert!(candidates.iter().any(|c| c.completion == "one"));

        for (text, expected) in [
            (L!("ssh -Q ci"), L!("pher")),
            (L!("cargo --ver"), L!("sion")),
        ] {
            let p = Projection::new(text, text.len(), parser.vars()).unwrap();
            let (candidates, _) = p
                .complete(CompletionRequestOptions::Completion, &mut parser.context())
                .unwrap();
            assert!(
                candidates.iter().any(|c| c.completion == expected),
                "{text}: {candidates:?}"
            );
        }
        {
            let cwd = std::env::current_dir().unwrap();
            let _cwd = fish_common::ScopeGuard::new(cwd, |cwd| {
                std::env::set_current_dir(cwd).unwrap();
            });
            std::fs::write(repo.join("Makefile"), "target_one:\n\t@:\n").unwrap();
            std::fs::write(
                repo.join("Cargo.toml"),
                "[package]\nname='probe-bin'\nversion='0.1.0'\nedition='2021'\n",
            )
            .unwrap();
            std::fs::create_dir(repo.join("src")).unwrap();
            std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
            std::fs::create_dir(repo.join("fixture-directory")).unwrap();
            std::fs::write(repo.join("fixture;$()` ü"), "").unwrap();
            std::env::set_current_dir(&repo).unwrap();
            parser.vars().set_pwd_from_getcwd();
            for (text, expected) in [
                (L!("make target_"), L!("one")),
                (L!("cargo run --bin probe-"), L!("bin")),
                (L!("cd fixture-d"), L!("irectory/")),
            ] {
                let p = Projection::new(text, text.len(), parser.vars()).unwrap();
                let (candidates, _) = p
                    .complete(CompletionRequestOptions::Completion, &mut parser.context())
                    .unwrap();
                assert!(
                    candidates.iter().any(|c| c.completion == expected),
                    "{text}: {candidates:?}"
                );
            }
            let text = L!("cat fixture");
            let p = Projection::new(text, text.len(), parser.vars()).unwrap();
            let (candidates, _) = p
                .complete(CompletionRequestOptions::Completion, &mut parser.context())
                .unwrap();
            let c = candidates
                .iter()
                .find(|c| c.completion == ";$()` ü")
                .unwrap();
            let (edit, _) = p.apply(c, false).unwrap();
            assert_eq!(edit, L!("cat fixture\\;\\$\\(\\)\\`\\ ü "));
        }
        parser.eval(
            L!("complete -c projection_option -l version -d Version"),
            &IoChain::new(),
        );
        let text = L!("projection_option --ver");
        let p = Projection::new(text, text.len(), parser.vars()).unwrap();
        let vars = parser.vars().snapshot();
        let (candidates, _) = p
            .complete(
                CompletionRequestOptions::Autosuggestion,
                &mut crate::operation_context::OperationContext::background_interruptible(&vars),
            )
            .unwrap();
        assert!(candidates.iter().any(|c| c.completion == "sion"));

        // Report paired warm in-process samples, excluding provider autoload and process startup.
        for text in [L!("git --ver"), L!("git -C \"$repo\" switch topic-")] {
            let mut direct = vec![];
            let mut projected = vec![];
            for _ in 0..25 {
                let start = std::time::Instant::now();
                let _ = complete(
                    text,
                    CompletionRequestOptions::Completion,
                    &mut parser.context(),
                );
                direct.push(start.elapsed().as_micros());
                let start = std::time::Instant::now();
                let p = Projection::new(text, text.len(), parser.vars()).unwrap();
                let _ = p
                    .complete(CompletionRequestOptions::Completion, &mut parser.context())
                    .unwrap();
                projected.push(start.elapsed().as_micros());
            }
            direct.sort();
            projected.sort();
            eprintln!(
                "completion latency us, {text}: direct p50={} p95={}; projected p50={} p95={}",
                direct[12], direct[23], projected[12], projected[23]
            );
        }
    }
}
