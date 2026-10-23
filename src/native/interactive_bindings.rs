//! User key bindings for the native Windows editor.
//!
//! Each single-key binding contains input functions or Fish callback scripts.

use rustyline::{Anchor, At, Cmd, KeyCode, KeyEvent, Modifiers, Movement, Word};
use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Bindings {
    entries: BTreeMap<Key, Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) enum Action {
    AcceptAutosuggestion,
    ForwardChar,
    BackwardChar,
    ForwardWord,
    BackwardWord,
    BeginningOfLine,
    EndOfLine,
    HistorySearchBackward,
    HistorySearchForward,
    DeleteChar,
    BackwardDeleteChar,
    KillWord,
    BackwardKillWord,
    KillLine,
    BackwardKillLine,
    Yank,
    Undo,
    Repaint,
    Execute,
    Complete,
}

impl Action {
    const ALL: [Self; 20] = [
        Self::AcceptAutosuggestion,
        Self::ForwardChar,
        Self::BackwardChar,
        Self::ForwardWord,
        Self::BackwardWord,
        Self::BeginningOfLine,
        Self::EndOfLine,
        Self::HistorySearchBackward,
        Self::HistorySearchForward,
        Self::DeleteChar,
        Self::BackwardDeleteChar,
        Self::KillWord,
        Self::BackwardKillWord,
        Self::KillLine,
        Self::BackwardKillLine,
        Self::Yank,
        Self::Undo,
        Self::Repaint,
        Self::Execute,
        Self::Complete,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::AcceptAutosuggestion => "accept-autosuggestion",
            Self::ForwardChar => "forward-char",
            Self::BackwardChar => "backward-char",
            Self::ForwardWord => "forward-word",
            Self::BackwardWord => "backward-word",
            Self::BeginningOfLine => "beginning-of-line",
            Self::EndOfLine => "end-of-line",
            Self::HistorySearchBackward => "history-search-backward",
            Self::HistorySearchForward => "history-search-forward",
            Self::DeleteChar => "delete-char",
            Self::BackwardDeleteChar => "backward-delete-char",
            Self::KillWord => "kill-word",
            Self::BackwardKillWord => "backward-kill-word",
            Self::KillLine => "kill-line",
            Self::BackwardKillLine => "backward-kill-line",
            Self::Yank => "yank",
            Self::Undo => "undo",
            Self::Repaint => "repaint",
            Self::Execute => "execute",
            Self::Complete => "complete",
        }
    }

    pub(super) fn parse(name: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|action| action.name() == name)
            .ok_or_else(|| {
                format!(
                    "bind: native bindings require one supported input function; use bind --function-names (unsupported: {name})"
                )
            })
    }

    /// Actions with Fish-specific state use the reader's existing handlers:
    /// history filtering, suggestion acceptance, completion, and Enter's
    /// abbreviation/multiline behavior. All other actions map directly.
    pub fn command(self) -> Option<Cmd> {
        Some(match self {
            Self::BackwardChar => Cmd::Move(Movement::BackwardChar(1)),
            Self::BackwardWord => Cmd::Move(Movement::BackwardWord(1, Word::Emacs)),
            Self::BeginningOfLine => Cmd::Move(Movement::BeginningOfLine),
            Self::DeleteChar => Cmd::Kill(Movement::ForwardChar(1)),
            Self::BackwardDeleteChar => Cmd::Kill(Movement::BackwardChar(1)),
            Self::KillWord => Cmd::Kill(Movement::ForwardWord(1, At::AfterEnd, Word::Emacs)),
            Self::BackwardKillWord => Cmd::Kill(Movement::BackwardWord(1, Word::Emacs)),
            Self::KillLine => Cmd::Kill(Movement::EndOfLine),
            Self::BackwardKillLine => Cmd::Kill(Movement::BeginningOfLine),
            Self::Yank => Cmd::Yank(1, Anchor::Before),
            Self::Undo => Cmd::Undo(1),
            Self::Repaint => Cmd::Repaint,
            Self::AcceptAutosuggestion
            | Self::ForwardChar
            | Self::ForwardWord
            | Self::EndOfLine
            | Self::HistorySearchBackward
            | Self::HistorySearchForward
            | Self::Execute
            | Self::Complete => return None,
        })
    }
}

impl Bindings {
    /// Return a snapshot to install after restoring the previous user bindings.
    /// The reader wraps every action with its completion-pager handler.
    pub fn entries(&self) -> Vec<(KeyEvent, Vec<String>)> {
        self.entries
            .iter()
            .map(|(key, actions)| (key.event(), actions.clone()))
            .collect()
    }

    pub fn run_builtin(&mut self, args: &[String]) -> Result<(i32, String), String> {
        #[derive(Clone, Copy, PartialEq)]
        enum Operation {
            Erase,
            Query,
            FunctionNames,
            KeyNames,
        }
        let mut operation = None;
        let mut all = false;
        let mut index = 0;
        while let Some(option) = args.get(index) {
            let selected = match option.as_str() {
                "-e" | "--erase" => Some(Operation::Erase),
                "-q" | "--query" | "-s" | "--silent" => Some(Operation::Query),
                "-f" | "--function-names" => Some(Operation::FunctionNames),
                "-K" | "--key-names" => Some(Operation::KeyNames),
                "-a" | "--all" => {
                    all = true;
                    None
                }
                "-ea" | "-ae" => {
                    all = true;
                    Some(Operation::Erase)
                }
                "--" => {
                    index += 1;
                    break;
                }
                _ if option.starts_with('-') && option != "-" => {
                    return Err(format!(
                        "bind: unsupported option {option}; native bindings support single keys"
                    ));
                }
                _ => break,
            };
            if let Some(selected) = selected {
                if operation.is_some_and(|previous| previous != selected) {
                    return Err("bind: operations cannot be combined".into());
                }
                operation = Some(selected);
            }
            index += 1;
        }
        let words = &args[index..];
        if all && operation != Some(Operation::Erase) {
            return Err("bind: --all requires --erase".into());
        }
        match operation {
            Some(Operation::FunctionNames | Operation::KeyNames) => {
                if !words.is_empty() {
                    return Err("bind: name listing does not accept arguments".into());
                }
                let mut names: Vec<String> = if operation == Some(Operation::FunctionNames) {
                    Action::ALL
                        .into_iter()
                        .map(|action| action.name().into())
                        .collect()
                } else {
                    KEY_NAMES
                        .iter()
                        .map(|name| (*name).into())
                        .chain((1..=12).map(|number| format!("f{number}")))
                        .collect()
                };
                names.sort();
                Ok((0, names.into_iter().map(|name| name + "\n").collect()))
            }
            Some(Operation::Erase) if all => {
                if !words.is_empty() {
                    return Err("bind: --erase --all does not accept keys".into());
                }
                self.entries.clear();
                Ok((0, String::new()))
            }
            Some(Operation::Erase | Operation::Query) => {
                if words.is_empty() {
                    return Err("bind: a query or erase operation requires a key".into());
                }
                // Validate every key before mutating the registry.
                let keys = words
                    .iter()
                    .map(|word| Key::parse(word))
                    .collect::<Result<Vec<_>, _>>()?;
                let missing = keys.iter().any(|key| !self.entries.contains_key(key));
                if operation == Some(Operation::Erase) {
                    for key in keys {
                        self.entries.remove(&key);
                    }
                }
                Ok((i32::from(missing), String::new()))
            }
            None if words.len() <= 1 => {
                let selected = words.first().map(|word| Key::parse(word)).transpose()?;
                let mut result = String::new();
                for (key, actions) in &self.entries {
                    if selected.as_ref().is_none_or(|selected| selected == key) {
                        writeln!(
                            result,
                            "bind -- {} {}",
                            quote(&key.name()),
                            actions
                                .iter()
                                .map(|action| quote(action))
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                        .unwrap();
                    }
                }
                Ok((i32::from(selected.is_some() && result.is_empty()), result))
            }
            None => {
                let key = Key::parse(&words[0])?;
                self.entries.insert(key, words[1..].to_vec());
                Ok((0, String::new()))
            }
        }
    }
}

const KEY_NAMES: &[&str] = &[
    "backspace",
    "delete",
    "down",
    "end",
    "enter",
    "escape",
    "home",
    "left",
    "pagedown",
    "pageup",
    "right",
    "space",
    "tab",
    "up",
];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
enum Code {
    Char(char),
    Backspace,
    BackTab,
    Delete,
    Down,
    End,
    Enter,
    Escape,
    Function(u8),
    Home,
    Left,
    PageDown,
    PageUp,
    Right,
    Tab,
    Up,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
struct Key {
    code: Code,
    control: bool,
    alt: bool,
    shift: bool,
}

impl Key {
    fn parse(source: &str) -> Result<Self, String> {
        let unsupported = || {
            format!(
                "bind: native bindings require one key such as ctrl-x, alt-x, or left; unsupported key: {}",
                source.escape_default()
            )
        };
        let mut chars = source.chars();
        if let Some(character) = chars.next() {
            if chars.next().is_none() {
                return Self::from_event(character_key(character)).ok_or_else(unsupported);
            }
        }
        // Legacy Fish \eX syntax is one Alt key, not a two-key binding.
        if let Some(tail) = source.strip_prefix('\x1b') {
            let mut chars = tail.chars();
            if let Some(character) = chars.next() {
                if chars.next().is_none() {
                    let mut event = character_key(character);
                    event.1 |= Modifiers::ALT;
                    return Self::from_event(event).ok_or_else(unsupported);
                }
            }
            return Err(unsupported());
        }
        let mut mods = Modifiers::NONE;
        let mut base = source;
        while let Some((prefix, tail)) = base.split_once('-') {
            let modifier = match prefix.to_ascii_lowercase().as_str() {
                "ctrl" => Modifiers::CTRL,
                "alt" => Modifiers::ALT,
                "shift" => Modifiers::SHIFT,
                _ => break,
            };
            if mods.contains(modifier) {
                return Err(unsupported());
            }
            mods |= modifier;
            base = tail;
        }
        let code = match base.to_ascii_lowercase().as_str() {
            "backspace" => KeyCode::Backspace,
            "backtab" => KeyCode::BackTab,
            "delete" => KeyCode::Delete,
            "down" => KeyCode::Down,
            "end" => KeyCode::End,
            "enter" | "return" => KeyCode::Enter,
            "escape" | "esc" => KeyCode::Esc,
            "home" => KeyCode::Home,
            "left" => KeyCode::Left,
            "pagedown" => KeyCode::PageDown,
            "pageup" => KeyCode::PageUp,
            "right" => KeyCode::Right,
            "space" => KeyCode::Char(' '),
            "tab" => KeyCode::Tab,
            "up" => KeyCode::Up,
            name if name
                .strip_prefix('f')
                .and_then(|number| number.parse::<u8>().ok())
                .is_some_and(|number| (1..=12).contains(&number)) =>
            {
                KeyCode::F(name[1..].parse().unwrap())
            }
            _ => {
                let mut chars = base.chars();
                let character = chars.next().ok_or_else(unsupported)?;
                if chars.next().is_some() || character.is_control() {
                    return Err(unsupported());
                }
                KeyCode::Char(
                    if mods.contains(Modifiers::SHIFT) && character.is_ascii_alphabetic() {
                        character.to_ascii_uppercase()
                    } else {
                        character
                    },
                )
            }
        };
        Self::from_event(KeyEvent(code, mods)).ok_or_else(unsupported)
    }

    fn from_event(event: KeyEvent) -> Option<Self> {
        let KeyEvent(code, mut mods) = KeyEvent::normalize(event);
        // BackTab already includes Shift. Avoid generating shift-shift-tab in
        // sourceable output for the explicit shift-backtab alias.
        if code == KeyCode::BackTab {
            mods.remove(Modifiers::SHIFT);
        }
        let code = match code {
            KeyCode::Char(character) => Code::Char(character),
            KeyCode::Backspace => Code::Backspace,
            KeyCode::BackTab => Code::BackTab,
            KeyCode::Delete => Code::Delete,
            KeyCode::Down => Code::Down,
            KeyCode::End => Code::End,
            KeyCode::Enter => Code::Enter,
            KeyCode::Esc => Code::Escape,
            KeyCode::F(number) => Code::Function(number),
            KeyCode::Home => Code::Home,
            KeyCode::Left => Code::Left,
            KeyCode::PageDown => Code::PageDown,
            KeyCode::PageUp => Code::PageUp,
            KeyCode::Right => Code::Right,
            KeyCode::Tab => Code::Tab,
            KeyCode::Up => Code::Up,
            _ => return None,
        };
        Some(Self {
            code,
            control: mods.contains(Modifiers::CTRL),
            alt: mods.contains(Modifiers::ALT),
            shift: mods.contains(Modifiers::SHIFT),
        })
    }

    fn event(&self) -> KeyEvent {
        let code = match self.code {
            Code::Char(character) => KeyCode::Char(character),
            Code::Backspace => KeyCode::Backspace,
            Code::BackTab => KeyCode::BackTab,
            Code::Delete => KeyCode::Delete,
            Code::Down => KeyCode::Down,
            Code::End => KeyCode::End,
            Code::Enter => KeyCode::Enter,
            Code::Escape => KeyCode::Esc,
            Code::Function(number) => KeyCode::F(number),
            Code::Home => KeyCode::Home,
            Code::Left => KeyCode::Left,
            Code::PageDown => KeyCode::PageDown,
            Code::PageUp => KeyCode::PageUp,
            Code::Right => KeyCode::Right,
            Code::Tab => KeyCode::Tab,
            Code::Up => KeyCode::Up,
        };
        let mut mods = Modifiers::NONE;
        if self.control {
            mods |= Modifiers::CTRL;
        }
        if self.alt {
            mods |= Modifiers::ALT;
        }
        if self.shift {
            mods |= Modifiers::SHIFT;
        }
        KeyEvent(code, mods)
    }

    fn name(&self) -> String {
        let mut name = String::new();
        if self.control {
            name.push_str("ctrl-");
        }
        if self.alt {
            name.push_str("alt-");
        }
        if self.shift {
            name.push_str("shift-");
        }
        match self.code {
            Code::Char(' ') => name.push_str("space"),
            Code::Char(character) if self.control && character.is_ascii_alphabetic() => {
                name.push(character.to_ascii_lowercase());
            }
            Code::Char(character) => name.push(character),
            Code::Backspace => name.push_str("backspace"),
            Code::BackTab => name.push_str("shift-tab"),
            Code::Delete => name.push_str("delete"),
            Code::Down => name.push_str("down"),
            Code::End => name.push_str("end"),
            Code::Enter => name.push_str("enter"),
            Code::Escape => name.push_str("escape"),
            Code::Function(number) => write!(name, "f{number}").unwrap(),
            Code::Home => name.push_str("home"),
            Code::Left => name.push_str("left"),
            Code::PageDown => name.push_str("pagedown"),
            Code::PageUp => name.push_str("pageup"),
            Code::Right => name.push_str("right"),
            Code::Tab => name.push_str("tab"),
            Code::Up => name.push_str("up"),
        }
        name
    }
}

fn character_key(character: char) -> KeyEvent {
    // Named terminal keys and their traditional control-character spelling are
    // interchangeable, while other controls retain their Ctrl-letter identity.
    match character {
        '\t' => KeyEvent(KeyCode::Tab, Modifiers::NONE),
        '\r' => KeyEvent(KeyCode::Enter, Modifiers::NONE),
        _ => KeyEvent::new(character, Modifiers::NONE),
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|word| (*word).into()).collect()
    }

    #[test]
    fn named_and_legacy_keys_normalize_to_the_same_registration() {
        for (name, legacy) in [
            ("ctrl-f", "\x06"),
            ("alt-x", "\x1bx"),
            ("tab", "\t"),
            ("enter", "\r"),
            ("escape", "\x1b"),
        ] {
            assert_eq!(Key::parse(name).unwrap(), Key::parse(legacy).unwrap());
        }
        assert_eq!(Key::parse("ctrl-F").unwrap(), Key::parse("ctrl-f").unwrap());
        assert_eq!(
            Key::parse("shift-tab").unwrap().event(),
            KeyEvent(KeyCode::BackTab, Modifiers::NONE)
        );
        assert_eq!(
            Key::parse("ctrl-alt-left").unwrap().event(),
            KeyEvent(KeyCode::Left, Modifiers::CTRL | Modifiers::ALT)
        );
        assert_eq!(Key::parse("🐟").unwrap().event(), KeyEvent::from('🐟'));
    }

    #[test]
    fn every_canonical_key_spelling_can_be_loaded_again() {
        for name in [
            "ctrl-a",
            "alt-x",
            "shift-tab",
            "ctrl-alt-right",
            "ctrl-\\",
            "'",
            "space",
            "shift-a",
            "alt-F",
            "f12",
            "shift-backtab",
            "escape",
            "ctrl-_",
        ] {
            let key = Key::parse(name).unwrap();
            assert_eq!(Key::parse(&key.name()).unwrap(), key, "{name}");
        }
    }

    #[test]
    fn registration_listing_query_replacement_and_erasure_use_normalized_keys() {
        let mut bindings = Bindings::default();
        bindings
            .run_builtin(&words(&["ctrl-x", "backward-char"]))
            .unwrap();
        assert_eq!(
            bindings.run_builtin(&words(&["--query", "\x18"])).unwrap(),
            (0, String::new())
        );
        assert_eq!(
            bindings.run_builtin(&words(&["ctrl-x"])).unwrap().1,
            "bind -- 'ctrl-x' 'backward-char'\n"
        );
        bindings.run_builtin(&words(&["\x18", "undo"])).unwrap();
        assert_eq!(
            bindings.entries(),
            vec![(
                KeyEvent::normalize(KeyEvent::ctrl('x')),
                vec!["undo".into()]
            )]
        );
        assert_eq!(
            bindings
                .run_builtin(&words(&["--erase", "ctrl-x"]))
                .unwrap()
                .0,
            0
        );
        assert_eq!(
            bindings
                .run_builtin(&words(&["--query", "ctrl-x"]))
                .unwrap()
                .0,
            1
        );
        bindings
            .run_builtin(&words(&["alt-x", "complete"]))
            .unwrap();
        bindings.run_builtin(&words(&["-ea"])).unwrap();
        assert!(bindings.entries().is_empty());
    }

    #[test]
    fn unsupported_key_sequences_and_modes_do_not_change_existing_bindings() {
        let mut bindings = Bindings::default();
        bindings.run_builtin(&words(&["ctrl-x", "undo"])).unwrap();
        for arguments in [
            vec!["ctrl-x,ctrl-e", "undo"],
            vec!["\x1b[A", "undo"],
            vec!["--mode", "insert", "ctrl-x", "undo"],
            vec!["--erase", "ctrl-x", "not-a-key"],
        ] {
            assert!(
                bindings.run_builtin(&words(&arguments)).is_err(),
                "{arguments:?}"
            );
        }
        assert_eq!(bindings.entries().len(), 1);
        assert_eq!(bindings.entries()[0].1, vec!["undo"]);
        bindings
            .run_builtin(&words(&["ctrl-x", "echo hello", "repaint"]))
            .unwrap();
        assert_eq!(bindings.entries()[0].1, vec!["echo hello", "repaint"]);
    }

    #[test]
    fn supported_actions_are_discoverable_and_special_actions_use_fish_handlers() {
        let mut bindings = Bindings::default();
        let names = bindings
            .run_builtin(&words(&["--function-names"]))
            .unwrap()
            .1;
        assert!(names.lines().any(|name| name == "accept-autosuggestion"));
        assert!(names.lines().any(|name| name == "history-search-backward"));
        for action in Action::ALL {
            assert_eq!(Action::parse(action.name()).unwrap(), action);
        }
        assert_eq!(
            Action::BackwardChar.command(),
            Some(Cmd::Move(Movement::BackwardChar(1)))
        );
        assert_eq!(
            Action::KillLine.command(),
            Some(Cmd::Kill(Movement::EndOfLine))
        );
        assert_eq!(Action::Undo.command(), Some(Cmd::Undo(1)));
        assert!(Action::Complete.command().is_none());
        assert!(Action::Execute.command().is_none());
        assert!(Action::HistorySearchBackward.command().is_none());
    }
}
