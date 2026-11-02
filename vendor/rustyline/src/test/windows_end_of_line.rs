use std::cell::Cell;

use crate::command;
use crate::completion::Completer;
use crate::config::{Config, EditMode};
use crate::edit::init_state;
use crate::highlight::Highlighter;
use crate::hint::{Hint, Hinter};
use crate::history::DefaultHistory;
use crate::keymap::{Bindings, Cmd, InputState, Movement, Refresher};
use crate::keys::KeyEvent as E;
use crate::kill_ring::KillRing;
use crate::tty::Sink;
use crate::validate::Validator;
use crate::{Context, Editor, Helper};

struct Suggestion {
    display: String,
    completion: Option<String>,
}

impl Hint for Suggestion {
    fn display(&self) -> &str {
        &self.display
    }

    fn completion(&self) -> Option<&str> {
        self.completion.as_deref()
    }
}

struct SuggestionHelper {
    suggestion: &'static str,
    pager: Cell<bool>,
}

impl SuggestionHelper {
    fn new(suggestion: &'static str) -> Self {
        Self {
            suggestion,
            pager: Cell::new(false),
        }
    }
}

impl Completer for SuggestionHelper {
    type Candidate = String;
}

impl Hinter for SuggestionHelper {
    type Hint = Suggestion;

    fn hint(&self, line: &str, pos: usize, _: &Context<'_>) -> Option<Suggestion> {
        if self.pager.get() {
            return Some(Suggestion {
                display: "display-only completion pager".into(),
                completion: None,
            });
        }
        if pos != line.len() {
            return None;
        }
        let completion = self.suggestion.strip_prefix(line)?;
        if completion.is_empty() {
            return None;
        }
        Some(Suggestion {
            // Accept completion text, never presentation-only decoration.
            display: format!("[suggestion: {completion}]"),
            completion: Some(completion.into()),
        })
    }
}

impl Helper for SuggestionHelper {}
impl Highlighter for SuggestionHelper {}
impl Validator for SuggestionHelper {}

fn assert_sequence(
    mode: EditMode,
    initial: (&str, &str),
    suggestion: &'static str,
    commands: Vec<Cmd>,
    expected: (&str, &str),
) {
    let config = Config::builder().edit_mode(mode).build();
    let mut editor = Editor::<SuggestionHelper, DefaultHistory>::with_config(config).unwrap();
    editor.set_helper(Some(SuggestionHelper::new(suggestion)));
    editor.term.keys = vec![E::ENTER];
    let actual = editor
        .readline_with_initial_command("", initial, Cmd::Sequence(commands))
        .unwrap();
    assert_eq!(actual, expected.0.to_owned() + expected.1);
    assert_eq!(editor.term.cursor, expected.0.len());
}

#[test]
fn queued_end_of_line_uses_replaced_input_and_is_one_undo() {
    for (undo_count, expected) in [(0, "git status 🐟"), (1, "git"), (2, "echo old")] {
        let mut commands = vec![
            Cmd::SetBuffer {
                buffer: "git".into(),
                cursor: 3,
            },
            Cmd::EndOfLineWithHint,
        ];
        commands.extend((0..undo_count).map(|_| Cmd::Undo(1)));
        assert_sequence(
            EditMode::Emacs,
            ("echo old", ""),
            "git status 🐟",
            commands,
            (expected, ""),
        );
    }
}

#[test]
fn queued_end_of_line_moves_then_accepts_the_updated_hint() {
    for (commands, expected) in [
        (vec![Cmd::EndOfLineWithHint], "git"),
        (
            vec![Cmd::EndOfLineWithHint, Cmd::EndOfLineWithHint],
            "git status",
        ),
        (
            vec![Cmd::Move(Movement::EndOfLine), Cmd::EndOfLineWithHint],
            "git status",
        ),
    ] {
        assert_sequence(
            EditMode::Emacs,
            ("g", "it"),
            "git status",
            commands,
            (expected, ""),
        );
    }
    assert_sequence(
        EditMode::Emacs,
        ("git", ""),
        "git status",
        vec![Cmd::Move(Movement::BackwardChar(1)), Cmd::EndOfLineWithHint],
        ("git", ""),
    );
}

#[test]
fn end_of_line_accepts_unicode_up_to_newline_in_emacs_and_vi_insert() {
    for mode in [EditMode::Emacs, EditMode::Vi] {
        for suggestion in ["echo Ω 🐟", "echo Ω 🐟\nsecond line"] {
            assert_sequence(
                mode,
                ("echo Ω", ""),
                suggestion,
                vec![
                    Cmd::EndOfLineWithHint,
                    Cmd::EndOfLineWithHint,
                    Cmd::SelfInsert(1, '!'),
                ],
                ("echo Ω 🐟!", ""),
            );
            assert_sequence(
                mode,
                ("echo Ω", ""),
                suggestion,
                vec![Cmd::EndOfLineWithHint, Cmd::Undo(1)],
                ("echo Ω", ""),
            );
        }
    }
}

#[test]
fn end_of_line_keeps_a_leading_hint_newline_unaccepted() {
    assert_sequence(
        EditMode::Emacs,
        ("first", ""),
        "first\nsecond",
        vec![Cmd::EndOfLineWithHint, Cmd::EndOfLineWithHint],
        ("first", ""),
    );
}

#[test]
fn end_of_line_without_hint_moves_only_to_the_current_line_end() {
    for commands in [
        vec![Cmd::EndOfLineWithHint],
        vec![Cmd::EndOfLineWithHint, Cmd::EndOfLineWithHint],
    ] {
        assert_sequence(
            EditMode::Emacs,
            ("fir", "st\nsecond"),
            "",
            commands,
            ("first", "\nsecond"),
        );
    }
}

#[test]
fn end_of_line_repaints_a_dismissed_pager_before_accepting_a_hint() {
    let mut out = Sink::default();
    let history = DefaultHistory::new();
    let helper = SuggestionHelper::new("git status");
    helper.pager.set(true);
    let mut state = init_state(&mut out, "git", 3, Some(&helper), &history);
    state.refresh_line().unwrap();
    let pager_end = state.layout.end;

    let config = Config::default();
    let bindings = Bindings::new();
    let input = InputState::new(&config, &bindings);
    let mut kill_ring = KillRing::new(60);
    helper.pager.set(false);
    command::execute(
        Cmd::EndOfLineWithHint,
        &mut state,
        &input,
        &mut kill_ring,
        &config,
    )
    .unwrap();
    assert_eq!(state.line.as_str(), "git");
    assert_eq!(state.line.pos(), 3);
    assert_eq!(state.hint_text(), Some(" status"));
    assert_ne!(state.layout.end, pager_end, "the pager must be redrawn");

    command::execute(
        Cmd::EndOfLineWithHint,
        &mut state,
        &input,
        &mut kill_ring,
        &config,
    )
    .unwrap();
    assert_eq!(state.line.as_str(), "git status");
}

#[test]
fn hint_acceptance_is_separate_from_an_open_vi_insert_undo_group() {
    for suggestion in ["git status Ω", "git status Ω\nnext command"] {
        let mut out = Sink::default();
        let history = DefaultHistory::new();
        let helper = SuggestionHelper::new(suggestion);
        let mut state = init_state(&mut out, "git ", 4, Some(&helper), &history);
        state.changes.begin();
        state.edit_insert('s', 1).unwrap();
        let config = Config::builder().edit_mode(EditMode::Vi).build();
        let bindings = Bindings::new();
        let input = InputState::new(&config, &bindings);
        let mut kill_ring = KillRing::new(60);
        command::execute(
            Cmd::EndOfLineWithHint,
            &mut state,
            &input,
            &mut kill_ring,
            &config,
        )
        .unwrap();
        assert_eq!(state.line.as_str(), "git status Ω");
        assert_eq!(state.line.pos(), "git status Ω".len());
        assert!(state.changes.undo(&mut state.line, 1));
        assert_eq!(state.line.as_str(), "git s");
        assert!(state.changes.undo(&mut state.line, 1));
        assert_eq!(state.line.as_str(), "git ");
    }
}
