//! Universal-variable scope and event integration for the native evaluator.
use super::*;

impl Session {
    pub(crate) fn initialize_universals(
        &mut self,
        path: std::path::PathBuf,
        host: &mut impl Host,
    ) -> Result<()> {
        self.universals = super::super::universal_variables::Store::new(path);
        self.refresh_universals(host)?;
        // A child inherits exported universal values as environment strings.
        // Preserve their universal identity instead of creating a permanent
        // global shadow. A distinct inherited value remains an override.
        self.variables.retain(|name, global| {
            !self.universals.vars.get(name).is_some_and(|universal| {
                universal.exported
                    && global.exported
                    && universal.exported_value() == global.exported_value()
            })
        });
        Ok(())
    }

    pub(super) fn refresh_universals(&mut self, host: &mut impl Host) -> Result<()> {
        if self.refreshing_universals {
            return Ok(());
        }
        self.refreshing_universals = true;
        let result = match self.universals.refresh() {
            Ok(changes) => self.universal_events(changes, None, host),
            Err(error) => {
                if let Some(message) = self.universals.diagnostic(&error) {
                    host.write(2, format!("fish: {message}\n").as_bytes(), &[], false)
                        .map(|_| ())
                } else {
                    Ok(())
                }
            }
        };
        self.refreshing_universals = false;
        result
    }

    fn universal_events(
        &mut self,
        changes: Vec<super::super::universal_variables::Change>,
        skip: Option<&str>,
        host: &mut impl Host,
    ) -> Result<()> {
        for change in changes {
            if skip == Some(change.name.as_str()) {
                continue;
            }
            if change.name == "fish_function_path" {
                self.refresh_function_paths();
            }
            self.fire_variable_event(&change.name, change.erased, host)?;
        }
        Ok(())
    }

    pub(super) fn set_universal(
        &mut self,
        name: &str,
        operation: impl FnOnce(Option<&Variable>) -> Option<Variable>,
        erase: bool,
        no_event: bool,
        host: &mut impl Host,
    ) -> Result<bool> {
        let (existed, remote_changes) =
            self.universals
                .update(name, operation)
                .map_err(|error| Error {
                    message: format!("set: universal variable {name}: {error}"),
                    status: 1,
                })?;
        self.universal_events(remote_changes, Some(name), host)?;
        if name == "fish_function_path" {
            self.refresh_function_paths();
        }
        if !no_event && (!erase || existed) {
            self.fire_variable_event(name, erase, host)?;
        }
        if erase {
            self.status = if existed { 0 } else { 4 };
        }
        Ok(!erase)
    }
}
