//! Fish-like navigation on top of the Windows line editor.
use super::interactive_pager::PagerBindings;
use rustyline::{
    At, Cmd, ConditionalEventHandler, Event, EventContext, Movement, RepeatCount, Word,
};
use std::sync::{Arc, Mutex};

pub(super) struct SuggestionKey {
    pub word: bool,
    pub pager: PagerBindings,
}

impl ConditionalEventHandler for SuggestionKey {
    fn handle(&self, _: &Event, n: RepeatCount, _: bool, ctx: &EventContext<'_>) -> Option<Cmd> {
        if ctx.mode() == rustyline::EditMode::Vi && ctx.input_mode() != rustyline::InputMode::Insert
        {
            return None;
        }
        Some(self.command(n, ctx))
    }
}

impl SuggestionKey {
    /// Explicit user bindings perform the named action in either vi mode;
    /// only the preset arrow bindings preserve vi's ordinary command map.
    pub(super) fn command(&self, n: RepeatCount, ctx: &EventContext<'_>) -> Cmd {
        let n = n.max(1);
        if ctx.pos() == ctx.line().len()
            && let Some(hint) = ctx.hint_text().filter(|hint| !hint.is_empty())
        {
            return if self.word {
                // Cmd::Insert uses vi's paste cursor rule even in insert
                // mode. Completion inserts at an exact byte position and
                // keeps the cursor after the accepted word.
                self.pager.queue_completion(
                    ctx.line(),
                    ctx.pos(),
                    ctx.pos(),
                    suggestion_word(hint).into(),
                )
            } else {
                Cmd::CompleteHint
            };
        }
        Cmd::Move(if self.word {
            Movement::ForwardWord(n, At::AfterEnd, Word::Emacs)
        } else {
            Movement::ForwardChar(n)
        })
    }
}

pub(super) struct EndOfLineKey {
    pub pager: PagerBindings,
}

impl ConditionalEventHandler for EndOfLineKey {
    fn handle(&self, _: &Event, _: RepeatCount, _: bool, ctx: &EventContext<'_>) -> Option<Cmd> {
        if ctx.mode() == rustyline::EditMode::Vi && ctx.input_mode() != rustyline::InputMode::Insert
        {
            return None;
        }
        Some(self.command(ctx))
    }
}

impl EndOfLineKey {
    pub(super) fn command(&self, ctx: &EventContext<'_>) -> Cmd {
        if ctx.pos() == ctx.line().len()
            && let Some(hint) = ctx.hint_text().filter(|hint| !hint.is_empty())
        {
            return match hint.split_once('\n') {
                Some(("", _)) => Cmd::Noop,
                Some((line, _)) => {
                    self.pager
                        .queue_completion(ctx.line(), ctx.pos(), ctx.pos(), line.into())
                }
                None => Cmd::CompleteHint,
            };
        }
        // The pager wrapper has dismissed a display-only menu. Moving at an
        // existing line end would not repaint, so explicitly erase its rows.
        if ctx.has_hint()
            && ctx.hint_text().is_none()
            && (ctx.pos() == ctx.line().len() || ctx.line()[ctx.pos()..].starts_with('\n'))
        {
            return Cmd::Repaint;
        }
        Cmd::Move(Movement::EndOfLine)
    }
}

fn suggestion_word(hint: &str) -> &str {
    let mut seen_word = false;
    for (at, ch) in hint.char_indices() {
        if ch.is_whitespace() && seen_word {
            return &hint[..at];
        }
        seen_word |= !ch.is_whitespace();
    }
    hint
}

#[derive(Default)]
pub(super) struct HistorySearch {
    pub entries: Vec<String>,
    draft: String,
    query: String,
    selected: Option<usize>,
    displayed: Option<String>,
}

impl HistorySearch {
    pub fn reset(&mut self, entries: Vec<String>) {
        *self = Self {
            entries,
            ..Self::default()
        };
    }

    fn search(&mut self, line: &str, reverse: bool) -> Option<String> {
        if self.displayed.as_deref() != Some(line) {
            self.draft = line.into();
            self.query = line.to_lowercase();
            self.selected = None;
        }
        let matches = |entry: &String| entry.to_lowercase().contains(&self.query);
        let selected = if reverse {
            let before = self.selected.unwrap_or(self.entries.len());
            (0..before)
                .rev()
                .find(|&index| matches(&self.entries[index]) && self.entries[index] != line)
        } else {
            let after = self.selected? + 1;
            (after..self.entries.len())
                .find(|&index| matches(&self.entries[index]) && self.entries[index] != line)
        };
        let replacement = if let Some(index) = selected {
            self.entries[index].clone()
        } else if reverse {
            return None;
        } else {
            self.draft.clone()
        };
        self.selected = selected;
        self.displayed = Some(replacement.clone());
        Some(replacement)
    }
}

pub(super) struct HistoryKey {
    pub search: Arc<Mutex<HistorySearch>>,
    pub reverse: bool,
}

impl ConditionalEventHandler for HistoryKey {
    fn handle(&self, _: &Event, _: RepeatCount, _: bool, ctx: &EventContext<'_>) -> Option<Cmd> {
        if ctx.mode() == rustyline::EditMode::Vi && ctx.input_mode() != rustyline::InputMode::Insert
        {
            return None;
        }
        // Inside a multiline edit arrows keep moving vertically. At its edge
        // they search history using the original input, including after recall.
        if (self.reverse && ctx.line()[..ctx.pos()].contains('\n'))
            || (!self.reverse && ctx.line()[ctx.pos()..].contains('\n'))
        {
            return None;
        }
        self.command(ctx.line())
    }
}

impl HistoryKey {
    /// A named history-search action searches history independently of the
    /// vi keymap and multiline arrow-navigation behavior.
    pub(super) fn command(&self, line: &str) -> Option<Cmd> {
        Some(match self.search.lock().ok()?.search(line, self.reverse) {
            Some(line) => Cmd::Replace(Movement::WholeBuffer, Some(line)),
            None => Cmd::Noop,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_search_filters_substrings_skips_duplicates_and_restores_draft() {
        let mut history = HistorySearch::default();
        history.reset(vec![
            "echo 🐟 first".into(),
            "unrelated".into(),
            "echo 🐟 second".into(),
            "echo 🐟 second".into(),
        ]);
        assert_eq!(
            history.search("🐟", true).as_deref(),
            Some("echo 🐟 second")
        );
        assert_eq!(
            history.search("echo 🐟 second", true).as_deref(),
            Some("echo 🐟 first")
        );
        assert_eq!(
            history.search("echo 🐟 first", false).as_deref(),
            Some("echo 🐟 second")
        );
        assert_eq!(
            history.search("echo 🐟 second", false).as_deref(),
            Some("🐟")
        );
        assert_eq!(history.search("🐟", false), None);
        assert_eq!(
            history.search("UNRELATED", true).as_deref(),
            Some("unrelated")
        );
    }

    #[test]
    fn suggestion_accepts_one_unicode_word_at_a_time() {
        assert_eq!(suggestion_word(" 🐟 world"), " 🐟");
        assert_eq!(suggestion_word("ial/path other"), "ial/path");
        assert_eq!(suggestion_word("\n next"), "\n next");
    }

    #[test]
    fn named_history_actions_search_multiline_substrings_and_restore_the_draft() {
        let search = Arc::new(Mutex::new(HistorySearch::default()));
        search
            .lock()
            .unwrap()
            .reset(vec!["begin\n    echo 🐟\nend".into()]);
        let reverse = HistoryKey {
            search: Arc::clone(&search),
            reverse: true,
        };
        let forward = HistoryKey {
            search,
            reverse: false,
        };
        assert_eq!(
            reverse.command("🐟\nend"),
            Some(Cmd::Replace(
                Movement::WholeBuffer,
                Some("begin\n    echo 🐟\nend".into())
            ))
        );
        assert_eq!(
            forward.command("begin\n    echo 🐟\nend"),
            Some(Cmd::Replace(Movement::WholeBuffer, Some("🐟\nend".into())))
        );
    }
}
