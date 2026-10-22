//! Named and variable events for the native evaluator.
//!
//! The shared function option parser describes which events to register. Event
//! execution belongs to each evaluator because it controls function scopes,
//! command status, cancellation, and output independently.
use super::*;

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) enum Description {
    Variable(String),
    Generic(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use rash::host::{ReadResult, RunResult};

    #[derive(Default)]
    struct Capture {
        bytes: Vec<u8>,
        cancel_on_write: bool,
        cancelled: bool,
    }

    impl Host for Capture {
        fn run_pipeline(&mut self, _: &[Command], _: bool, _: bool) -> Result<RunResult> {
            Err(Error::new("unexpected external command in event test"))
        }

        fn change_dir(&mut self, _: &[u8]) -> Result<Vec<u8>> {
            Ok(b"C:/event-test".to_vec())
        }

        fn read_line(&mut self, _: &[Redirection], _: bool) -> Result<ReadResult> {
            Err(Error::new("unexpected read in event test"))
        }

        fn write(
            &mut self,
            _: i32,
            bytes: &[u8],
            _: &[Redirection],
            capture: bool,
        ) -> Result<Vec<u8>> {
            if capture {
                return Ok(bytes.to_vec());
            }
            self.bytes.extend_from_slice(bytes);
            self.cancelled |= self.cancel_on_write;
            Ok(Vec::new())
        }

        fn cancelled(&self) -> bool {
            self.cancelled
        }

        fn jobs(&self) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }

    #[test]
    fn pwd_handlers_remain_in_the_interactive_session() {
        let mut session = Session::new().unwrap();
        session.set_mode(true, false);
        let mut host = Capture::default();
        session.eval(
            "function directory --on-variable PWD; status --is-interactive; or return; echo interactive $argv; end; builtin cd .",
            &mut host,
        ).unwrap();
        assert_eq!(host.bytes, b"interactive VARIABLE SET PWD\n");
    }

    #[test]
    fn cancellation_stops_later_handlers_and_the_triggering_command() {
        for trigger in ["emit test", "watched=value builtin echo unreachable"] {
            let mut session = Session::new().unwrap();
            let mut host = Capture {
                cancel_on_write: true,
                ..Default::default()
            };
            let status = session.eval(
                &format!("function first --on-event test --on-variable watched; echo first; end; function later --on-event test --on-variable watched; echo later; end; {trigger}; echo after"),
                &mut host,
            ).unwrap();
            assert_eq!(status, 130);
            assert_eq!(host.bytes, b"first\n");
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Registration {
    pub(super) id: u64,
    pub(super) description: Description,
}

impl Description {
    pub(super) fn option(&self) -> (&str, &str) {
        match self {
            Self::Variable(name) => ("--on-variable", name),
            Self::Generic(name) => ("--on-event", name),
        }
    }
}

impl Session {
    pub(super) fn register_events(
        &mut self,
        descriptions: Vec<Description>,
    ) -> Result<Vec<Registration>> {
        let end = self
            .next_event_id
            .checked_add(descriptions.len() as u64)
            .ok_or_else(|| Error::new("too many event handler registrations"))?;
        let registrations = descriptions
            .into_iter()
            .zip(self.next_event_id..end)
            .map(|(description, id)| Registration { id, description })
            .collect();
        self.next_event_id = end;
        Ok(registrations)
    }

    pub(super) fn fire_variable_event(
        &mut self,
        name: &str,
        erased: bool,
        host: &mut impl Host,
    ) -> Result<()> {
        self.fire_event(
            &Description::Variable(name.into()),
            &[
                "VARIABLE".into(),
                if erased { "ERASE" } else { "SET" }.into(),
                name.into(),
            ],
            host,
        )
    }

    pub(super) fn fire_event(
        &mut self,
        description: &Description,
        arguments: &[String],
        host: &mut impl Host,
    ) -> Result<()> {
        // Functions are stored by name, but events run in registration order.
        // IDs also distinguish a removed registration from a new definition of
        // the same function created by an earlier handler during this dispatch.
        let mut handlers: Vec<_> = self
            .functions
            .iter()
            .flat_map(|(name, function)| {
                function
                    .events
                    .iter()
                    .filter(|event| &event.description == description)
                    .map(move |event| (event.id, name.clone()))
            })
            .collect();
        handlers.sort_unstable_by_key(|(id, _)| *id);
        for (id, name) in handlers {
            if self.flow != Flow::Normal || host.cancelled() {
                break;
            }
            let Some(function) = self
                .functions
                .get(&name)
                .filter(|function| function.events.iter().any(|event| event.id == id))
                .cloned()
            else {
                continue;
            };
            let status = self.status;
            let pipeline = self.pipeline.clone();
            let input_mode_blocked = std::mem::replace(&mut self.input_mode_blocked, true);
            // Like POSIX events, handlers do not inherit the triggering
            // command's redirections or command-substitution capture. Actual
            // process handles (including a worker's pipeline) still apply.
            let mut output = Output::default();
            let result = self.invoke_function(&name, function, arguments, host, &mut output);
            self.input_mode_blocked = input_mode_blocked;
            self.status = status;
            self.pipeline = pipeline;
            if let Err(error) = result {
                host.write(
                    2,
                    format!("fish: {}\n", error.message).as_bytes(),
                    &[],
                    false,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn emit(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let mut args = args;
        if args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "-h" | "--help"))
        {
            self.write(b"emit EVENT_NAME [ARGUMENTS ...]\n", host, output)?;
            self.status = 0;
            return Ok(());
        }
        if args.first().is_some_and(|arg| arg == "--") {
            args = &args[1..];
        } else if args
            .first()
            .is_some_and(|arg| arg.starts_with('-') && arg != "-")
        {
            return Err(Error::new(format!("emit: {}: unknown option", args[0])));
        }
        let Some((name, arguments)) = args.split_first() else {
            return Err(Error::new("emit: expected event name"));
        };
        self.fire_event(&Description::Generic(name.clone()), arguments, host)?;
        self.status = 0;
        Ok(())
    }
}
