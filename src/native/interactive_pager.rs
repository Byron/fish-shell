//! A searchable completion menu for the native reader.
//!
//! The menu is a non-insertable Rustyline hint. Its terminal backend therefore
//! owns all row clearing, wrapping and cursor positioning, including after a
//! resize. Navigation never edits the input; accepting a candidate is one edit.

use super::interactive_runtime::CompletionService;
use rustyline::completion::Pair;
use rustyline::hint::Hint;
use rustyline::{
    Cmd, ConditionalEventHandler, Event, EventContext, EventHandler, KeyCode, KeyEvent, Modifiers,
    RepeatCount,
};
use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

/// Keep display-only menu rows out of suggestion acceptance and the command.
pub(super) struct EditorHint {
    text: String,
    insertable: bool,
}

impl EditorHint {
    pub(super) fn suggestion(text: String) -> Self {
        Self {
            text,
            insertable: true,
        }
    }
}

impl Hint for EditorHint {
    fn display(&self) -> &str {
        &self.text
    }

    fn completion(&self) -> Option<&str> {
        self.insertable.then_some(&self.text)
    }
}

/// Clone this into every explicit binding and the `Event::Any` fallback.
#[derive(Clone)]
pub(super) struct PagerBindings {
    service: Arc<Mutex<CompletionService>>,
    state: Arc<Mutex<Option<Pager>>>,
    pending: Arc<Mutex<Option<PendingCompletion>>>,
}

struct PendingCompletion {
    original: String,
    pos: usize,
    start: usize,
    replacement: String,
}

impl PagerBindings {
    pub(super) fn new(service: Arc<Mutex<CompletionService>>) -> Self {
        Self {
            service,
            state: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn reset(&self) {
        *self.state.lock().unwrap() = None;
        *self.pending.lock().unwrap() = None;
    }

    /// Use Rustyline's completion update for an exact byte-range replacement.
    /// Custom `Cmd::Replace` bindings have their movement count rewritten by
    /// Rustyline's repeat handling, so they cannot reliably replace a token.
    pub(super) fn queue_completion(
        &self,
        line: &str,
        pos: usize,
        start: usize,
        replacement: String,
    ) -> Cmd {
        if start > pos || !line.is_char_boundary(start) || !line.is_char_boundary(pos) {
            return Cmd::Noop;
        }
        *self.pending.lock().unwrap() = Some(PendingCompletion {
            original: line.into(),
            pos,
            start,
            replacement,
        });
        Cmd::Complete
    }

    /// The helper consumes this before calculating ordinary completions.
    pub(super) fn take_completion(&self, line: &str, pos: usize) -> Option<(usize, Vec<Pair>)> {
        let pending = self.pending.lock().ok()?.take()?;
        if pending.original != line || pending.pos != pos {
            return None;
        }
        Some((
            pending.start,
            vec![Pair {
                display: pending.replacement.clone(),
                replacement: pending.replacement,
            }],
        ))
    }

    /// Wrap an existing binding, giving the open menu first refusal.
    pub(super) fn wrap(&self, fallback: Option<EventHandler>) -> EventHandler {
        EventHandler::Conditional(Box::new(PagerKey {
            pager: self.clone(),
            fallback,
            open_on_tab: true,
        }))
    }

    /// An explicit user binding supersedes the default Tab action. An already
    /// open menu still receives its navigation and cancellation keys first.
    pub(super) fn wrap_user(&self, fallback: EventHandler) -> EventHandler {
        EventHandler::Conditional(Box::new(PagerKey {
            pager: self.clone(),
            fallback: Some(fallback),
            open_on_tab: false,
        }))
    }

    pub(super) fn hint(&self, line: &str, pos: usize) -> Option<EditorHint> {
        let mut state = self.state.lock().ok()?;
        let pager = state.as_ref()?;
        // A binding supplied by the user may edit the buffer without consulting
        // the menu. Never apply an old completion to that new input.
        if pager.original != line || pager.pos != pos {
            *state = None;
            return None;
        }
        let (columns, rows) = terminal_size();
        Some(EditorHint {
            text: pager.render(columns, rows),
            insertable: false,
        })
    }

    pub(super) fn highlight_hint<'a>(&self, hint: &'a str) -> Option<Cow<'a, str>> {
        self.state.lock().ok()?.as_ref()?;
        let mut output = String::with_capacity(hint.len() + 50);
        for (index, line) in hint.split('\n').enumerate() {
            if index > 0 {
                output.push('\n');
            }
            if line.starts_with("> ") {
                output.push_str("\x1b[7m");
                output.push_str(line);
                output.push_str("\x1b[0m");
            } else if index == 1 {
                output.push_str("\x1b[36m");
                output.push_str(line);
                output.push_str("\x1b[0m");
            } else {
                output.push_str(line);
            }
        }
        Some(Cow::Owned(output))
    }

    pub(super) fn handle(&self, event: &Event, ctx: &EventContext<'_>) -> Option<Cmd> {
        self.handle_key(*event.get(0)?, ctx.line(), ctx.pos())
    }

    fn handle_key(&self, key: KeyEvent, line: &str, pos: usize) -> Option<Cmd> {
        self.handle_key_with_open(key, line, pos, true)
    }

    fn handle_key_with_open(
        &self,
        key: KeyEvent,
        line: &str,
        pos: usize,
        open_on_tab: bool,
    ) -> Option<Cmd> {
        let mut state = self.state.lock().ok()?;
        if state
            .as_ref()
            .is_some_and(|pager| pager.original != line || pager.pos != pos)
        {
            *state = None;
        }
        let backwards = matches!(key, KeyEvent(KeyCode::BackTab, _))
            || key == KeyEvent(KeyCode::Tab, Modifiers::SHIFT);
        let tab = backwards
            || key == KeyEvent(KeyCode::Tab, Modifiers::NONE)
            || key == KeyEvent::ctrl('I');
        if state.is_none() {
            if !tab || !open_on_tab {
                return None;
            }
            // Evaluation may emit output or invoke nested complete -C. Keep
            // pager state unlocked while the service borrows the evaluator.
            drop(state);
            let cwd = std::env::current_dir().ok()?;
            let (start, mut candidates) = match self.service.lock().ok()?.complete(line, pos, &cwd)
            {
                Ok(result) => result,
                Err(_) => return Some(Cmd::Interrupt),
            };
            return Some(match candidates.len() {
                0 => Cmd::Noop,
                1 => self.queue_completion(line, pos, start, candidates.remove(0).replacement),
                _ => {
                    let mut pager = Pager::new(line, pos, start, candidates);
                    if backwards {
                        pager.navigate(-1);
                    }
                    *self.state.lock().ok()? = Some(pager);
                    Cmd::Repaint
                }
            });
        }
        let pager = state.as_mut().unwrap();
        if tab {
            pager.navigate(if backwards { -1 } else { 1 });
            return Some(Cmd::Repaint);
        }
        match key {
            KeyEvent(KeyCode::Down | KeyCode::Right, Modifiers::NONE)
            | KeyEvent(KeyCode::Char('N'), Modifiers::CTRL) => pager.navigate(1),
            KeyEvent(KeyCode::Up | KeyCode::Left, Modifiers::NONE)
            | KeyEvent(KeyCode::Char('P'), Modifiers::CTRL) => pager.navigate(-1),
            KeyEvent(KeyCode::PageDown, Modifiers::NONE) => {
                pager.navigate(page_size(terminal_size().1) as isize);
            }
            KeyEvent(KeyCode::PageUp, Modifiers::NONE) => {
                pager.navigate(-(page_size(terminal_size().1) as isize));
            }
            KeyEvent(KeyCode::Home, Modifiers::NONE) => {
                pager.selected = (!pager.matches.is_empty()).then_some(0);
            }
            KeyEvent(KeyCode::End, Modifiers::NONE) => {
                pager.selected = pager.matches.len().checked_sub(1);
            }
            KeyEvent(KeyCode::Enter, Modifiers::NONE)
            | KeyEvent(KeyCode::Char('J' | 'M'), Modifiers::CTRL) => {
                let command = pager.accept().map_or(Cmd::Repaint, |(start, replacement)| {
                    self.queue_completion(line, pos, start, replacement)
                });
                *state = None;
                return Some(command);
            }
            KeyEvent(KeyCode::Esc, Modifiers::NONE)
            | KeyEvent(KeyCode::Char('G'), Modifiers::CTRL) => {
                *state = None;
            }
            KeyEvent(KeyCode::Backspace, Modifiers::NONE)
            | KeyEvent(KeyCode::Char('H'), Modifiers::CTRL) => {
                if let Some((offset, _)) = pager.query.grapheme_indices(true).next_back() {
                    pager.query.truncate(offset);
                    pager.filter();
                }
            }
            KeyEvent(KeyCode::Char('U'), Modifiers::CTRL) => {
                pager.query.clear();
                pager.filter();
            }
            KeyEvent(KeyCode::Char(ch), modifiers)
                if (modifiers.is_empty() || modifiers == Modifiers::SHIFT) && !ch.is_control() =>
            {
                pager.query.push(ch);
                pager.filter();
            }
            // Interrupt, undo, paste and modified navigation retain their normal
            // meaning. The input has not changed, so cancellation is lossless.
            _ => {
                *state = None;
                return None;
            }
        }
        Some(Cmd::Repaint)
    }
}

struct PagerKey {
    pager: PagerBindings,
    fallback: Option<EventHandler>,
    open_on_tab: bool,
}

impl ConditionalEventHandler for PagerKey {
    fn handle(
        &self,
        event: &Event,
        count: RepeatCount,
        positive: bool,
        ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        self.pager
            .handle_key_with_open(*event.get(0)?, ctx.line(), ctx.pos(), self.open_on_tab)
            .or_else(|| match &self.fallback {
                Some(EventHandler::Simple(command)) => Some(command.clone()),
                Some(EventHandler::Conditional(handler)) => {
                    handler.handle(event, count, positive, ctx)
                }
                None => None,
            })
    }
}

struct Pager {
    original: String,
    pos: usize,
    start: usize,
    candidates: Vec<Pair>,
    matches: Vec<usize>,
    selected: Option<usize>,
    query: String,
}

impl Pager {
    fn new(line: &str, pos: usize, start: usize, candidates: Vec<Pair>) -> Self {
        Self {
            original: line.into(),
            pos,
            start,
            matches: (0..candidates.len()).collect(),
            candidates,
            selected: None,
            query: String::new(),
        }
    }

    fn filter(&mut self) {
        let query = self.query.to_lowercase();
        self.matches = self
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                // Descriptions explain candidates but must not manufacture a
                // match by extending a partial name ("b" in "zebra", "t" in
                // "description"). Replacement text contains only the value;
                // fuzzy matching naturally skips any Fish quoting escapes.
                fuzzy_contains(&candidate.replacement.to_lowercase(), &query).then_some(index)
            })
            .collect();
        self.selected = (!self.matches.is_empty()).then_some(0);
    }

    fn navigate(&mut self, delta: isize) {
        let count = self.matches.len() as isize;
        if count == 0 {
            return;
        }
        self.selected = Some(match self.selected {
            Some(index) => (index as isize + delta).rem_euclid(count) as usize,
            None if delta < 0 => self.matches.len() - 1,
            None => 0,
        });
    }

    fn accept(&self) -> Option<(usize, String)> {
        let index = *self.matches.get(self.selected.unwrap_or(0))?;
        Some((self.start, self.candidates[index].replacement.clone()))
    }

    fn render(&self, columns: usize, rows: usize) -> String {
        // Leave the final column unused to avoid an additional wrapped row.
        let width = columns.saturating_sub(1).max(1);
        let count = self.matches.len();
        let header = format!(
            "Search: {}_  ({}/{})  Tab/arrows/Ctrl-N/P select, Enter accept, Esc cancel",
            self.query,
            count,
            self.candidates.len()
        );
        let mut output = format!("\n{}", truncate(&visible_text(&header), width));
        if count == 0 {
            output.push('\n');
            output.push_str(&truncate("  No matches", width));
            return output;
        }
        let size = page_size(rows);
        let selected = self.selected.unwrap_or(0);
        let start = selected / size * size;
        let end = (start + size).min(count);
        for index in start..end {
            let prefix = if self.selected == Some(index) {
                "> "
            } else {
                "  "
            };
            let text = format!(
                "{prefix}{}",
                visible_text(&self.candidates[self.matches[index]].display)
            );
            output.push('\n');
            output.push_str(&truncate(&text, width));
        }
        if count > size {
            output.push('\n');
            output.push_str(&truncate(
                &format!("  {}-{end} of {count}", start + 1),
                width,
            ));
        }
        output
    }
}

fn fuzzy_contains(text: &str, query: &str) -> bool {
    let mut haystack = text.chars();
    query.chars().all(|needle| haystack.any(|ch| ch == needle))
}

fn visible_text(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.into();
    }
    let mut used = 0;
    let mut output = String::new();
    // Keep the truncation marker single-width in both ConPTY and terminal
    // emulators with different East Asian ambiguous-width preferences.
    let marker = if width >= 3 { "..." } else { "." };
    for grapheme in text.graphemes(true) {
        let columns = grapheme.width();
        if used + columns > width.saturating_sub(marker.len()) {
            break;
        }
        used += columns;
        output.push_str(grapheme);
    }
    output.push_str(marker);
    output
}

fn page_size(rows: usize) -> usize {
    (rows / 2).saturating_sub(3).clamp(1, 8)
}

fn terminal_size() -> (usize, usize) {
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } != 0 {
        (
            info.dwSize.X.max(1) as usize,
            (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as usize,
        )
    } else {
        (80, 24)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<Pair> {
        ["alpha", "beta", "東京", "zebra"]
            .into_iter()
            .map(|name| Pair {
                display: format!("{name}  (description)"),
                replacement: format!("{name} "),
            })
            .collect()
    }

    fn open_pager(line: &str, pos: usize, start: usize) -> PagerBindings {
        let bindings = PagerBindings::new(Arc::new(Mutex::new(CompletionService::new("fish"))));
        *bindings.state.lock().unwrap() = Some(Pager::new(line, pos, start, candidates()));
        bindings
    }

    #[test]
    fn filtering_enter_escape_and_paste_do_not_submit_or_lose_the_draft() {
        let line = "run a suffix";
        let bindings = open_pager(line, 5, 4);
        for ch in ['B', 'T'] {
            assert_eq!(
                bindings.handle_key(KeyEvent(KeyCode::Char(ch), Modifiers::NONE), line, 5),
                Some(Cmd::Repaint)
            );
        }
        assert_eq!(
            bindings.handle_key(KeyEvent(KeyCode::Enter, Modifiers::NONE), line, 5),
            Some(Cmd::Complete)
        );
        let (start, candidates) = bindings.take_completion(line, 5).unwrap();
        assert_eq!(start, 4);
        assert_eq!(candidates[0].replacement, "beta ");
        assert!(bindings.state.lock().unwrap().is_none());

        let bindings = open_pager(line, 5, 4);
        assert_eq!(
            bindings.handle_key(KeyEvent(KeyCode::Esc, Modifiers::NONE), line, 5),
            Some(Cmd::Repaint)
        );
        assert!(bindings.state.lock().unwrap().is_none());

        let bindings = open_pager(line, 5, 4);
        assert_eq!(
            bindings.handle_key(
                KeyEvent(KeyCode::BracketedPasteStart, Modifiers::NONE),
                line,
                5
            ),
            None
        );
        assert!(bindings.state.lock().unwrap().is_none());
    }

    #[test]
    fn user_tab_overrides_run_until_a_menu_is_open() {
        let bindings = open_pager("run ", 4, 4);
        let tabs = [
            KeyEvent(KeyCode::Tab, Modifiers::NONE),
            KeyEvent(KeyCode::BackTab, Modifiers::NONE),
            KeyEvent::ctrl('I'),
        ];
        for key in tabs {
            assert_eq!(
                bindings.handle_key_with_open(key, "run ", 4, false),
                Some(Cmd::Repaint)
            );
        }
        bindings.reset();
        for key in tabs {
            assert_eq!(bindings.handle_key_with_open(key, "run ", 4, false), None);
            assert!(bindings.state.lock().unwrap().is_none());
        }
        // The default Tab action can still start completing after an override
        // has been erased, and an explicit `complete` action uses this path.
        assert!(bindings.handle_key(tabs[0], "ec", 2).is_some());
    }

    #[test]
    fn backspace_removes_a_complete_filter_grapheme_and_stale_input_cancels() {
        let bindings = open_pager("run ", 4, 4);
        bindings.state.lock().unwrap().as_mut().unwrap().query = "東e\u{301}".into();
        assert_eq!(
            bindings.handle_key(KeyEvent(KeyCode::Backspace, Modifiers::NONE), "run ", 4),
            Some(Cmd::Repaint)
        );
        assert_eq!(bindings.state.lock().unwrap().as_ref().unwrap().query, "東");
        assert_eq!(
            bindings.handle_key(
                KeyEvent(KeyCode::Enter, Modifiers::NONE),
                "run different",
                13
            ),
            None
        );
        assert!(bindings.state.lock().unwrap().is_none());
    }

    #[test]
    fn menu_wraps_selection_filters_and_preserves_original_input() {
        let line = "echo 🐟; run a suffix";
        let start = line.find("a suffix").unwrap();
        let pos = start + 1;
        let mut pager = Pager::new(line, pos, start, candidates());
        assert_eq!(pager.selected, None);
        pager.navigate(-1);
        assert_eq!(pager.selected, Some(3));
        pager.navigate(1);
        assert_eq!(pager.selected, Some(0));
        pager.query = "BT".into();
        pager.filter();
        assert_eq!(pager.matches, [1]);
        assert_eq!(pager.original, line);
        assert_eq!(pager.pos, pos);
        assert_eq!(pager.accept(), Some((start, "beta ".into())));
        pager.query = "missing".into();
        pager.filter();
        assert_eq!(pager.accept(), None);
        assert!(pager.render(80, 24).contains("No matches"));
    }

    #[test]
    fn menu_hint_can_never_be_accepted_as_an_autosuggestion() {
        let hint = EditorHint {
            text: "\n> candidate".into(),
            insertable: false,
        };
        assert_eq!(hint.completion(), None);
        assert_eq!(
            EditorHint::suggestion("tail".into()).completion(),
            Some("tail")
        );
    }

    #[test]
    fn queued_replacement_preserves_byte_range_cursor_and_suffix() {
        let bindings = open_pager("run ", 4, 4);
        for (line, pos, start, expected) in [
            (
                "echo 👩‍💻e\u{301} suffix",
                "echo 👩‍💻e\u{301}".len(),
                5,
                "echo done  suffix",
            ),
            ("echo $FEEL_var", 14, 6, "echo $done "),
            ("run  suffix", 4, 4, "run done  suffix"),
        ] {
            assert_eq!(
                bindings.queue_completion(line, pos, start, "done ".into()),
                Cmd::Complete
            );
            let (start, candidates) = bindings.take_completion(line, pos).unwrap();
            assert_eq!(candidates.len(), 1);
            let mut actual = line.to_owned();
            actual.replace_range(start..pos, &candidates[0].replacement);
            assert_eq!(actual, expected);
            assert!(bindings.take_completion(line, pos).is_none());
        }
        bindings.queue_completion("run original", 12, 4, "done ".into());
        assert!(bindings.take_completion("run changed", 11).is_none());
        assert!(bindings.take_completion("run original", 12).is_none());
        assert_eq!(bindings.queue_completion("🐟", 1, 0, "a".into()), Cmd::Noop);
    }

    #[test]
    fn layout_bounds_rows_width_and_escapes_untrusted_display_text() {
        let mut pager = Pager::new("run ", 4, 4, candidates());
        pager.candidates[0].display = "東京 👩‍💻e\u{301} \x1b[2J\n\t".into();
        pager.navigate(1);
        let rendered = pager.render(16, 10);
        assert!(rendered.lines().all(|line| line.width() < 16));
        assert!(rendered.lines().count() <= 5);
        let full = pager.render(200, 24);
        assert!(!full.contains('\x1b'));
        assert!(full.contains("\\u{1b}[2J\\n\\t"));
        assert!(full.contains("東京 👩‍💻e\u{301}"));
        assert_eq!(truncate("東京", 2), ".");
    }
}
