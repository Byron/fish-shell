//! Native evaluation adapter for Fish's shared function discovery and cache.
use super::*;
use crate::autoload::{Asset, Autoload, AutoloadPath, AutoloadResult};
use crate::function_options::valid_function_name;
use crate::parser_keywords::parser_keywords_is_reserved;
use fish_widestring::L;

pub(super) fn resolver() -> Autoload {
    Autoload::new(L!("fish_function_path"))
}

pub(super) mod serialization {
    use super::*;
    use serde::{Deserialize as _, Serialize as _};

    pub(crate) fn serialize<S: serde::Serializer>(
        loader: &Autoload,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        loader.snapshot().serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Autoload, D::Error> {
        let state = crate::autoload::AutoloadState::deserialize(deserializer)?;
        let mut loader = resolver();
        loader.restore(state);
        Ok(loader)
    }
}

pub(super) fn read_source(path: AutoloadPath) -> Result<(String, String)> {
    match path {
        AutoloadPath::OnDisk(path) => {
            let origin = path.to_string();
            let source = std::fs::read_to_string(&origin)?;
            Ok((origin, source))
        }
        AutoloadPath::Embedded(path) => {
            let asset = Asset::get(&path)
                .ok_or_else(|| Error::new(format!("autoload source missing: {path}")))?;
            let source = std::str::from_utf8(&asset.data)
                .map_err(|_| Error::new("autoload source must be UTF-8"))?
                .to_owned();
            Ok((format!("embedded:{path}"), source))
        }
    }
}

fn absolute_directory(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(name)
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
}

impl Session {
    pub(super) fn initialize_function_paths(&mut self) {
        let home = absolute_directory("HOME").or_else(|| absolute_directory("USERPROFILE"));
        let data = absolute_directory("XDG_DATA_HOME")
            .or_else(|| absolute_directory("LOCALAPPDATA"))
            .or_else(|| home.as_ref().map(|home| home.join(".local/share")))
            .map(|directory| directory.join("fish"));
        let cache = absolute_directory("XDG_CACHE_HOME")
            .or_else(|| absolute_directory("LOCALAPPDATA"))
            .or_else(|| home.as_ref().map(|home| home.join(".cache")))
            .map(|directory| directory.join("fish"));
        let mut vendor_data = Vec::new();
        if std::env::var_os("FISH_UNIT_TESTS_RUNNING").is_none() {
            vendor_data.extend(data.iter().cloned());
            if let Some(directories) = std::env::var_os("XDG_DATA_DIRS") {
                vendor_data.extend(
                    std::env::split_paths(&directories)
                        .filter(|directory| directory.is_absolute())
                        .map(|directory| directory.join("fish")),
                );
            }
        }
        let vendor_functions: Vec<_> = vendor_data
            .iter()
            .map(|directory| directory.join("vendor_functions.d"))
            .collect();
        let vendor_completions: Vec<_> = vendor_data
            .iter()
            .map(|directory| directory.join("vendor_completions.d"))
            .collect();
        for (name, directories) in [
            ("__fish_user_data_dir", data.into_iter().collect()),
            ("__fish_cache_dir", cache.iter().cloned().collect()),
            ("__fish_vendor_functionsdirs", vendor_functions.clone()),
            ("__fish_vendor_completionsdirs", vendor_completions.clone()),
        ] {
            self.variables.insert(
                name.into(),
                Variable {
                    values: directories
                        .iter()
                        .map(|directory| directory.to_string_lossy().into_owned())
                        .collect(),
                    exported: false,
                    pathvar: false,
                },
            );
        }
        let mut function_paths = Vec::new();
        let mut completion_paths = Vec::new();
        if let Some(directory) = super::super::interactive::config_directory("fish") {
            self.variables.insert(
                "__fish_config_dir".into(),
                Variable {
                    values: vec![directory.to_string_lossy().into_owned()],
                    exported: false,
                    pathvar: false,
                },
            );
            function_paths.push(directory.join("functions"));
            completion_paths.push(directory.join("completions"));
        }
        function_paths.extend(vendor_functions);
        completion_paths.extend(vendor_completions);
        completion_paths.extend(cache.map(|directory| directory.join("generated_completions")));
        for (name, directories) in [
            ("fish_function_path", function_paths),
            ("fish_complete_path", completion_paths),
        ] {
            self.variables
                .entry(name.into())
                .or_insert_with(|| Variable {
                    values: directories
                        .iter()
                        .map(|directory| directory.to_string_lossy().into_owned())
                        .collect(),
                    exported: false,
                    pathvar: false,
                });
        }
    }

    fn function_paths(&self) -> Vec<WString> {
        self.values("fish_function_path")
            .into_iter()
            .map(WString::from)
            .collect()
    }

    pub(super) fn refresh_function_paths(&mut self) {
        let paths = self.values("fish_function_path");
        if paths != self.autoload_paths {
            for name in std::mem::take(&mut self.autoloaded_functions) {
                self.functions.remove(&name);
            }
            self.function_autoload.clear();
            self.autoload_paths = paths;
        }
    }

    pub(super) fn function_names(&self) -> Vec<String> {
        let mut names: std::collections::BTreeSet<_> =
            crate::autoload::function_names(&self.function_paths(), true)
                .into_iter()
                .map(|name| name.to_string())
                .filter(|name| !self.autoload_tombstones.contains(name))
                .collect();
        names.extend(
            self.functions
                .keys()
                .filter(|name| {
                    self.autoload_paths == self.values("fish_function_path")
                        || !self.autoloaded_functions.contains(*name)
                })
                .cloned(),
        );
        names.into_iter().collect()
    }

    pub fn completion_wrappers(&self) -> Vec<(String, Vec<String>)> {
        self.functions
            .iter()
            .filter(|(name, _)| {
                self.autoload_paths == self.values("fish_function_path")
                    || !self.autoloaded_functions.contains(*name)
            })
            .filter(|(_, function)| !function.wrap_targets.is_empty())
            .map(|(name, function)| (name.clone(), function.wrap_targets.clone()))
            .collect()
    }

    pub(super) fn ensure_function(
        &mut self,
        name: &str,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<bool> {
        self.refresh_function_paths();
        let wide = WString::from(name);
        if !valid_function_name(&wide) || parser_keywords_is_reserved(&wide) {
            return Ok(false);
        }
        if self.functions.contains_key(name) && !self.autoloaded_functions.contains(name) {
            return Ok(true);
        }
        if self.autoload_tombstones.contains(name) {
            return Ok(false);
        }
        let paths = self.function_paths();
        // Native implementations of builtins/editor commands take the place of
        // their bundled POSIX shims. User function files can still override them.
        // Likewise, retain fish> unless a user supplies a fish_prompt function.
        let embedded = !builtins::is_builtin(name)
            && name != "fish_prompt"
            && !(name == "seq" && crate::native::utilities::available("seq"));
        let found = self
            .function_autoload
            .resolve_command_from_paths_with_embedded(&wide, &paths, embedded);
        if let AutoloadResult::Path(path) = found {
            let status = self.status;
            let pipeline = self.pipeline.clone();
            let flow = self.flow;
            self.flow = Flow::Normal;
            self.functions.remove(name);
            self.autoloaded_functions.insert(name.to_owned());
            let result = (|| {
                let (origin, source) = read_source(path)?;
                self.eval_sourced(&source, origin, &[], true, host, output)
            })();
            self.function_autoload.mark_autoload_finished(&wide);
            if self.flow != Flow::Exit {
                self.status = status;
                self.pipeline = pipeline;
                self.flow = flow;
            }
            result?;
        }
        Ok(self.functions.contains_key(name))
    }
}
