//! Native key sequences use the existing binding trie before built-in dispatch.

use super::*;
use crate::error::ReadlineError;
use radix_trie::TrieCommon as _;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub(super) struct SequenceState {
    pending: VecDeque<KeyEvent>,
    timeout: Duration,
}

impl SequenceState {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            pending: VecDeque::new(),
            timeout: Duration::from_millis(u64::from(config.keyseq_timeout().unwrap_or(500))),
        }
    }
}

/// Replayed lookahead must also reach built-ins which read another key, such
/// as Ctrl-X Ctrl-U and quoted insert. Keep it in the reader, not a command list.
struct ReplayReader<'r, R: ?Sized> {
    inner: &'r mut R,
    pending: VecDeque<KeyEvent>,
}

impl<R: RawReader + ?Sized> ReplayReader<'_, R> {
    fn prepend(&mut self, keys: &[KeyEvent]) {
        for key in keys.iter().rev() {
            self.pending.push_front(*key);
        }
    }
}

impl<R: RawReader + ?Sized> RawReader for ReplayReader<'_, R> {
    type Buffer = R::Buffer;

    fn wait_for_input(&mut self, single_esc_abort: bool) -> Result<tty::Event> {
        match self.pending.pop_front() {
            Some(key) => Ok(tty::Event::KeyPress(key)),
            None => self.inner.wait_for_input(single_esc_abort),
        }
    }

    fn wait_for_input_timeout(
        &mut self,
        single_esc_abort: bool,
        timeout: Duration,
    ) -> Result<Option<tty::Event>> {
        match self.pending.pop_front() {
            Some(key) => Ok(Some(tty::Event::KeyPress(key))),
            None => self.inner.wait_for_input_timeout(single_esc_abort, timeout),
        }
    }

    fn next_key(&mut self, single_esc_abort: bool) -> Result<KeyEvent> {
        match self.pending.pop_front() {
            Some(key) => Ok(key),
            None => self.inner.next_key(single_esc_abort),
        }
    }

    fn buffer_keys(&mut self, keys: &mut VecDeque<KeyEvent>) {
        while let Some(key) = keys.pop_back() {
            self.pending.push_front(key);
        }
    }

    fn read_pasted_text(&mut self) -> Result<String> {
        self.inner.read_pasted_text()
    }

    fn find_binding(&self, key: &KeyEvent) -> Option<Cmd> {
        self.inner.find_binding(key)
    }

    fn unbuffer(self) -> Option<Self::Buffer> {
        // This adapter only borrows the actual reader. InputState keeps its
        // lookahead between calls, and the real reader owns terminal typeahead.
        None
    }
}

impl InputState<'_> {
    pub(super) fn next_windows_cmd<R: RawReader>(
        &mut self,
        rdr: &mut R,
        wrt: &mut dyn Refresher,
        single_esc_abort: bool,
        ignore_external_print: bool,
    ) -> Result<Cmd> {
        let mut replay = ReplayReader {
            inner: rdr,
            pending: std::mem::take(&mut self.sequence.pending),
        };
        let result =
            self.resolve_sequence(&mut replay, wrt, single_esc_abort, ignore_external_print);
        // AcceptLine may finish this InputState immediately. Return lookahead
        // to the console's persistent typeahead buffer before handing it back.
        replay.inner.buffer_keys(&mut replay.pending);
        self.sequence.pending = replay.pending;
        result
    }

    fn has_longer_binding(&self, event: &Event) -> bool {
        self.custom_bindings
            .get_raw_descendant(event)
            .is_some_and(|descendant| descendant.key() != Some(event) || !descendant.is_leaf())
    }

    fn resolve_sequence<R: RawReader>(
        &mut self,
        rdr: &mut ReplayReader<'_, R>,
        wrt: &mut dyn Refresher,
        single_esc_abort: bool,
        ignore_external_print: bool,
    ) -> Result<Cmd> {
        let first = self.read_key(rdr, wrt, single_esc_abort, ignore_external_print)?;
        self.resolve_sequence_from_first(rdr, wrt, first)
    }

    /// Numeric-prefix parsing has already read the first key of the command.
    /// Resolve its sequence before consuming the repeat count. Erasing the
    /// inner reader type keeps re-entry through built-in dispatch bounded.
    pub(super) fn repeated_sequence<R: RawReader>(
        &mut self,
        rdr: &mut R,
        wrt: &mut dyn Refresher,
        first: KeyEvent,
    ) -> Result<Cmd> {
        let mut replay = ReplayReader {
            inner: rdr as &mut dyn RawReader<Buffer = R::Buffer>,
            pending: VecDeque::new(),
        };
        let result = self.resolve_sequence_from_first(&mut replay, wrt, first);
        replay.inner.buffer_keys(&mut replay.pending);
        self.sequence.pending.extend(replay.pending);
        result
    }

    fn resolve_sequence_from_first<R: RawReader + ?Sized>(
        &mut self,
        rdr: &mut ReplayReader<'_, R>,
        wrt: &mut dyn Refresher,
        first: KeyEvent,
    ) -> Result<Cmd> {
        let mut keys = vec![KeyEvent::normalize(first)];
        if !self.has_longer_binding(&Event::KeySeq(keys.clone())) {
            return self.command_for_key(rdr, wrt, first);
        }

        let mut longest = None;
        loop {
            let event = Event::KeySeq(keys.clone());
            if self.custom_bindings.get(&event).is_some() {
                longest = Some(keys.len());
            }
            if !self.has_longer_binding(&event) {
                break;
            }

            // Only ambiguity needs a timeout. A prefix with no shorter binding
            // can wait for a deliberately typed chord, like native Ctrl-X.
            let deadline = longest.map(|_| Instant::now() + self.sequence.timeout);
            let next = loop {
                let next = match deadline {
                    Some(deadline) => rdr.wait_for_input_timeout(
                        true,
                        deadline.saturating_duration_since(Instant::now()),
                    ),
                    None => rdr.wait_for_input(true).map(Some),
                };
                match next {
                    Ok(Some(tty::Event::ExternalPrint(message))) => {
                        wrt.external_print(message)?;
                    }
                    Err(ReadlineError::WindowResized) => {
                        // The outer editor repaints and calls us again. Retain
                        // the prefix, and let the console retain partial VT data.
                        rdr.prepend(&keys);
                        return Err(ReadlineError::WindowResized);
                    }
                    other => break other?,
                }
            };
            let Some(tty::Event::KeyPress(key)) = next else {
                break;
            };
            let key = KeyEvent::normalize(key);
            keys.push(key);
            // Explicit bindings may use the usual cancellation keys. Reserve
            // cancellation for a key which cannot continue the current chord.
            if self
                .custom_bindings
                .get_raw_descendant(&Event::KeySeq(keys.clone()))
                .is_none()
            {
                match key {
                    E::ESC | E(K::Char('G'), M::CTRL) => return Ok(Cmd::Abort),
                    E(K::Char('C'), M::CTRL) => return Ok(Cmd::Interrupt),
                    _ => (),
                }
            }
        }

        if let Some(length) = longest {
            let event = Event::KeySeq(keys[..length].to_vec());
            if let Some(command) = self.sequence_command(&event, wrt) {
                rdr.prepend(&keys[length..]);
                return Ok(command);
            }
        }

        // No custom binding accepted the input. Replay lookahead so ordinary
        // characters, control keys and bracketed paste keep their usual meaning.
        rdr.prepend(&keys[1..]);
        if keys[0] == KeyEvent::ctrl('X')
            && !matches!(
                keys.get(1).copied(),
                Some(E(K::Char('U'), M::CTRL) | E::BACKSPACE)
            )
        {
            // Ctrl-X has no action of its own. Its built-in fallback would eat
            // an unmatched second key (including a bracketed-paste marker).
            return Ok(Cmd::Noop);
        }
        self.command_for_key(rdr, wrt, first)
    }

    fn sequence_command(&mut self, event: &Event, wrt: &dyn Refresher) -> Option<Cmd> {
        let (count, positive, repeat) = match self.mode {
            EditMode::Emacs => {
                let (count, positive) = self.emacs_num_args();
                (count, positive, Some(count))
            }
            EditMode::Vi if self.input_mode == InputMode::Command => {
                let count = self.vi_num_args();
                (count, true, Some(count))
            }
            EditMode::Vi => (0, true, None),
        };
        let command = match self.custom_bindings.get(event)? {
            EventHandler::Simple(command) => Some(command.clone()),
            EventHandler::Conditional(handler) => {
                handler.handle(event, count, positive, &EventContext::new(self, wrt))
            }
        }?;
        Some(if command.is_repeatable() {
            command.redo(repeat, wrt)
        } else {
            command
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::init_state;
    use crate::history::DefaultHistory;
    use crate::tty::Sink;

    enum Input {
        Key(KeyEvent),
        Timeout,
        Resize,
    }

    struct Reader(VecDeque<Input>);

    impl RawReader for Reader {
        type Buffer = ();

        fn wait_for_input(&mut self, _: bool) -> Result<tty::Event> {
            match self.0.pop_front() {
                Some(Input::Key(key)) => Ok(tty::Event::KeyPress(key)),
                Some(Input::Resize) => Err(ReadlineError::WindowResized),
                Some(Input::Timeout) => panic!("unambiguous prefixes do not use a timeout"),
                None => Err(ReadlineError::Eof),
            }
        }

        fn wait_for_input_timeout(
            &mut self,
            single: bool,
            _: Duration,
        ) -> Result<Option<tty::Event>> {
            if matches!(self.0.front(), Some(Input::Timeout)) {
                self.0.pop_front();
                Ok(None)
            } else {
                self.wait_for_input(single).map(Some)
            }
        }

        fn next_key(&mut self, single: bool) -> Result<KeyEvent> {
            match self.wait_for_input(single)? {
                tty::Event::KeyPress(key) => Ok(key),
                tty::Event::ExternalPrint(_) => unreachable!(),
            }
        }

        fn read_pasted_text(&mut self) -> Result<String> {
            Ok("pasted\ntext".into())
        }

        fn find_binding(&self, _: &KeyEvent) -> Option<Cmd> {
            None
        }

        fn unbuffer(self) -> Option<()> {
            None
        }
    }

    fn bindings(definitions: &[(&[KeyEvent], &str)]) -> Bindings {
        let mut bindings = Bindings::new();
        for (keys, value) in definitions {
            bindings.insert(
                Event::KeySeq(keys.to_vec()).normalize(),
                EventHandler::Simple(Cmd::Insert(1, (*value).into())),
            );
        }
        bindings
    }

    fn check(mode: EditMode, bindings: &Bindings, inputs: Vec<Input>, expected: &[Cmd]) {
        let config = Config::builder()
            .edit_mode(mode)
            .keyseq_timeout(Some(17))
            .build();
        let mut input = InputState::new(&config, bindings);
        assert_eq!(input.sequence.timeout, Duration::from_millis(17));
        let history = DefaultHistory::new();
        let mut sink = Sink::default();
        let mut state = init_state(&mut sink, "", 0, None::<&()>, &history);
        let mut reader = Reader(inputs.into());
        for command in expected {
            assert_eq!(
                input
                    .next_windows_cmd(&mut reader, &mut state, false, false)
                    .unwrap(),
                *command,
            );
        }
        assert!(reader.0.is_empty());
        assert!(input.sequence.pending.is_empty());
    }

    #[test]
    fn arbitrary_prefixes_and_longer_bindings_work_in_emacs_and_vi_insert() {
        let table = bindings(&[
            (&[KeyEvent::ctrl('X'), KeyEvent::ctrl('E')], "editor"),
            (&[KeyEvent::alt('q'), 'a'.into(), 'b'.into()], "three"),
            (&[KeyEvent::ctrl('T')], "short"),
            (&[KeyEvent::ctrl('T'), KeyEvent::ctrl('R')], "long"),
        ]);
        for mode in [EditMode::Emacs, EditMode::Vi] {
            check(
                mode,
                &table,
                [
                    KeyEvent::ctrl('X'),
                    KeyEvent::ctrl('E'),
                    KeyEvent::alt('q'),
                    'a'.into(),
                    'b'.into(),
                    KeyEvent::ctrl('T'),
                    KeyEvent::ctrl('R'),
                ]
                .into_iter()
                .map(Input::Key)
                .collect(),
                &[
                    Cmd::Insert(1, "editor".into()),
                    Cmd::Insert(1, "three".into()),
                    Cmd::Insert(1, "long".into()),
                ],
            );
        }
    }

    #[test]
    fn ambiguous_shorter_binding_runs_on_timeout_and_preserves_mismatched_suffix() {
        let table = bindings(&[
            (&[KeyEvent::ctrl('T')], "short"),
            (&[KeyEvent::ctrl('T'), 'a'.into(), 'b'.into()], "long"),
        ]);
        check(
            EditMode::Emacs,
            &table,
            vec![
                Input::Key(KeyEvent::ctrl('T')),
                Input::Timeout,
                Input::Key(KeyEvent::ctrl('T')),
                Input::Key('a'.into()),
                Input::Key('z'.into()),
            ],
            &[
                Cmd::Insert(1, "short".into()),
                Cmd::Insert(1, "short".into()),
                Cmd::SelfInsert(1, 'a'),
                Cmd::SelfInsert(1, 'z'),
            ],
        );
    }

    #[test]
    fn unmatched_prefixes_preserve_characters_and_builtin_ctrl_x_fallbacks() {
        let table = bindings(&[
            (&[KeyEvent::ctrl('X'), KeyEvent::ctrl('E')], "editor"),
            (&['j'.into(), 'k'.into()], "pair"),
        ]);
        check(
            EditMode::Emacs,
            &table,
            [
                KeyEvent::ctrl('X'),
                'z'.into(),
                'j'.into(),
                'q'.into(),
                KeyEvent::ctrl('X'),
                KeyEvent::ctrl('U'),
            ]
            .into_iter()
            .map(Input::Key)
            .collect(),
            &[
                Cmd::Noop,
                Cmd::SelfInsert(1, 'z'),
                Cmd::SelfInsert(1, 'j'),
                Cmd::SelfInsert(1, 'q'),
                Cmd::Undo(1),
            ],
        );
    }

    #[test]
    fn escape_and_control_g_cancel_while_control_c_interrupts() {
        let table = bindings(&[(&[KeyEvent::ctrl('X'), KeyEvent::ctrl('E')], "editor")]);
        for mode in [EditMode::Emacs, EditMode::Vi] {
            check(
                mode,
                &table,
                [
                    KeyEvent::ctrl('X'),
                    E::ESC,
                    KeyEvent::ctrl('X'),
                    KeyEvent::ctrl('G'),
                    KeyEvent::ctrl('X'),
                    KeyEvent::ctrl('C'),
                ]
                .into_iter()
                .map(Input::Key)
                .collect(),
                &[Cmd::Abort, Cmd::Abort, Cmd::Interrupt],
            );
        }
    }

    #[test]
    fn registered_cancel_keys_and_their_continuations_are_reachable() {
        let table = bindings(&[
            (&[KeyEvent::ctrl('X'), E::ESC], "escape"),
            (&[KeyEvent::ctrl('X'), E::ESC, 'a'.into()], "extended"),
            (&[KeyEvent::ctrl('X'), KeyEvent::ctrl('G')], "control-g"),
            (&[KeyEvent::ctrl('X'), KeyEvent::ctrl('C')], "control-c"),
        ]);
        for mode in [EditMode::Emacs, EditMode::Vi] {
            check(
                mode,
                &table,
                vec![
                    Input::Key(KeyEvent::ctrl('X')),
                    Input::Key(E::ESC),
                    Input::Timeout,
                    Input::Key(KeyEvent::ctrl('X')),
                    Input::Key(E::ESC),
                    Input::Key('a'.into()),
                    Input::Key(KeyEvent::ctrl('X')),
                    Input::Key(KeyEvent::ctrl('G')),
                    Input::Key(KeyEvent::ctrl('X')),
                    Input::Key(KeyEvent::ctrl('C')),
                ],
                &[
                    Cmd::Insert(1, "escape".into()),
                    Cmd::Insert(1, "extended".into()),
                    Cmd::Insert(1, "control-g".into()),
                    Cmd::Insert(1, "control-c".into()),
                ],
            );
        }
    }

    #[test]
    fn emacs_numeric_prefix_repeats_sequences_and_shorter_fallbacks() {
        let table = bindings(&[
            (&[KeyEvent::ctrl('X'), KeyEvent::ctrl('E')], "editor"),
            (&[KeyEvent::ctrl('T')], "short"),
            (&[KeyEvent::ctrl('T'), KeyEvent::ctrl('R')], "long"),
        ]);
        check(
            EditMode::Emacs,
            &table,
            [
                KeyEvent::alt('2'),
                KeyEvent::ctrl('X'),
                KeyEvent::ctrl('E'),
                KeyEvent::alt('3'),
                KeyEvent::ctrl('T'),
                'z'.into(),
            ]
            .into_iter()
            .map(Input::Key)
            .collect(),
            &[
                Cmd::Insert(2, "editor".into()),
                Cmd::Insert(3, "short".into()),
                Cmd::SelfInsert(1, 'z'),
            ],
        );
    }

    #[test]
    fn bracketed_paste_after_a_prefix_keeps_its_marker_and_contents() {
        let table = bindings(&[(&[KeyEvent::ctrl('X'), KeyEvent::ctrl('E')], "editor")]);
        check(
            EditMode::Emacs,
            &table,
            vec![
                Input::Key(KeyEvent::ctrl('X')),
                Input::Key(E(K::BracketedPasteStart, M::NONE)),
            ],
            &[Cmd::Noop, Cmd::Insert(1, "pasted\ntext".into())],
        );
    }

    #[test]
    fn resize_preserves_the_prefix_and_eof_returns_without_panicking() {
        let table = bindings(&[(&[KeyEvent::ctrl('X'), KeyEvent::ctrl('E')], "editor")]);
        let config = Config::default();
        let mut input = InputState::new(&config, &table);
        let history = DefaultHistory::new();
        let mut sink = Sink::default();
        let mut state = init_state(&mut sink, "", 0, None::<&()>, &history);
        let mut reader = Reader(
            vec![
                Input::Key(KeyEvent::ctrl('X')),
                Input::Resize,
                Input::Key(KeyEvent::ctrl('E')),
                Input::Key(KeyEvent::ctrl('X')),
            ]
            .into(),
        );
        assert!(matches!(
            input.next_windows_cmd(&mut reader, &mut state, false, false),
            Err(ReadlineError::WindowResized)
        ));
        assert_eq!(
            input
                .next_windows_cmd(&mut reader, &mut state, false, false)
                .unwrap(),
            Cmd::Insert(1, "editor".into())
        );
        assert!(matches!(
            input.next_windows_cmd(&mut reader, &mut state, false, false),
            Err(ReadlineError::Eof)
        ));
    }

    #[test]
    fn lookahead_survives_a_submitting_binding_and_a_new_input_state() {
        let mut table = bindings(&[(&[KeyEvent::ctrl('O'), KeyEvent::ctrl('E')], "long")]);
        table.insert(
            KeyEvent::ctrl('O').into(),
            EventHandler::Simple(Cmd::AcceptLine),
        );
        let config = Config::default();
        let history = DefaultHistory::new();
        let mut sink = Sink::default();
        let mut state = init_state(&mut sink, "", 0, None::<&()>, &history);
        let mut reader = vec![KeyEvent::ctrl('O'), 'z'.into()].into_iter();
        assert_eq!(
            InputState::new(&config, &table)
                .next_windows_cmd(&mut reader, &mut state, false, false)
                .unwrap(),
            Cmd::AcceptLine,
        );
        assert_eq!(
            InputState::new(&config, &table)
                .next_windows_cmd(&mut reader, &mut state, false, false)
                .unwrap(),
            Cmd::SelfInsert(1, 'z'),
        );
    }
}
