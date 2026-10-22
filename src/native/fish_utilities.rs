//! Resolve against shell-visible PATH without exporting local variables.

use super::*;
use crate::native::{command_resolution, utilities};
use std::ffi::{OsStr, OsString};
use std::io;

impl Session {
    pub(super) fn command_search_environment(
        &self,
    ) -> io::Result<(Option<OsString>, Option<OsString>)> {
        let path = self
            .variable("PATH")
            .filter(|var| !var.values.is_empty())
            .map(|var| std::env::join_paths(&var.values))
            .transpose()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let pathext = self
            .variable("PATHEXT")
            .filter(|var| !var.values.is_empty())
            .map(|var| var.values.join(";").into());
        Ok((path, pathext))
    }

    pub(super) fn external_argv(&self, name: &str, args: &[String]) -> io::Result<Vec<Vec<u8>>> {
        let (path, pathext) = self.command_search_environment()?;
        let selected =
            command_resolution::first(OsStr::new(name), path.as_deref(), pathext.as_deref())?;
        let arguments: Vec<OsString> = args.iter().map(OsString::from).collect();
        let argv = if let Some(path) = selected {
            std::iter::once(path.into_os_string())
                .chain(arguments)
                .collect()
        } else if !command_resolution::is_explicit(OsStr::new(name)) && utilities::available(name) {
            utilities::invocation(name, &arguments)?
        } else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "command not found"));
        };
        argv.iter()
            .map(|value| {
                rash::os::to_bytes(value)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.message))
            })
            .collect()
    }

    pub(super) fn native_utility(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        if args == ["--is-enabled"] {
            self.status = i32::from(!cfg!(feature = "native-utilities"));
            return Ok(());
        }
        let Some((name, args)) = args.split_first() else {
            return Err(Error::new("__fish_native_utility: expected a utility name"));
        };
        let args: Vec<_> = args.iter().map(OsString::from).collect();
        let invocation = utilities::invocation(name, &args)?;
        let mut invocation = invocation
            .into_iter()
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| Error::new("native executable path must be valid UTF-8"))
            })
            .collect::<Result<Vec<_>>>()?;
        let executable = invocation.remove(0);
        self.external(&executable, &invocation, host, output)
    }
}
