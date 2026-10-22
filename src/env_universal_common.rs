use crate::env::{EnvVar, EnvVarFlags, VarTable};
use crate::flog::{flog, flogf};
use crate::fs::{PotentialUpdate, lock_and_load, rewrite_via_temporary_file};
use crate::path::{ValidatedPath, path_get_config};
use crate::prelude::*;
use crate::wutil::{FileId, file_id_for_file, file_id_for_path_narrow, wrealpath};
use fish_widestring::wcs2zstring;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read as _, Write as _};

/// Callback data, reflecting a change in universal variables.
#[derive(Clone, Debug, PartialEq)]
pub struct CallbackData {
    // The name of the variable.
    pub key: WString,

    // The value of the variable, or none if it is erased.
    pub val: Option<EnvVar>,
}

pub type CallbackDataList = Vec<CallbackData>;

use crate::universal_variables::{Format as UvarFormat, MAX_READ_SIZE};

/// Class representing universal variables.
pub struct EnvUniversal {
    // Path that we save to. This is set in initialize(). If empty, initialize has not been called.
    vars_path: WString,
    narrow_vars_path: CString,

    // The table of variables.
    vars: VarTable,

    // Keys that have been modified, and need to be written. A value here that is not present in
    // vars indicates a deleted value.
    modified: HashSet<WString>,

    // A generation count which is incremented every time an exported variable is modified.
    export_generation: u64,

    // Whether it's OK to save. This may be set to false if we discover that a future version of
    // fish wrote the uvars contents.
    // Only update if last_read_file_id is updated as well.
    ok_to_save: bool,

    // File id from which we last read.
    // Only update if ok_to_save is updated as well.
    last_read_file_id: Option<FileId>,
}

struct UniversalReadUpdate {
    export_generation_increment: u64,
    new_vars: VarTable,
    callbacks: CallbackDataList,
    ok_to_save: bool,
}

impl EnvUniversal {
    // Construct an empty universal variables.
    pub fn new() -> Self {
        Self {
            vars_path: Default::default(),
            narrow_vars_path: Default::default(),
            vars: Default::default(),
            modified: Default::default(),
            export_generation: 1,
            ok_to_save: true,
            last_read_file_id: None,
        }
    }
    // Get the value of the variable with the specified name.
    pub fn get(&self, name: &wstr) -> Option<EnvVar> {
        self.vars.get(name).cloned()
    }
    // Return flags from the variable with the given name.
    pub fn get_flags(&self, name: &wstr) -> Option<EnvVarFlags> {
        self.vars.get(name).map(|var| var.flags())
    }
    // Sets a variable.
    pub fn set(&mut self, key: &wstr, var: EnvVar) {
        let exports = var.exports();
        match self.vars.entry(key.to_owned()) {
            Entry::Occupied(mut entry) => {
                if entry.get() == &var {
                    return;
                }
                entry.insert(var);
            }
            Entry::Vacant(entry) => {
                entry.insert(var);
            }
        }
        self.modified.insert(key.to_owned());
        if exports {
            self.export_generation += 1;
        }
    }
    // Removes a variable. Returns true if it was found, false if not.
    pub fn remove(&mut self, key: &wstr) -> bool {
        if let Some(var) = self.vars.remove(key) {
            if var.exports() {
                self.export_generation += 1;
            }
            self.modified.insert(key.to_owned());
            return true;
        }
        false
    }

    // Gets variable names.
    pub fn get_names(&self, show_exported: bool, show_unexported: bool) -> Vec<WString> {
        let mut result = vec![];
        for (key, var) in &self.vars {
            if (var.exports() && show_exported) || (!var.exports() && show_unexported) {
                result.push(key.clone());
            }
        }
        result
    }

    /// Get a view on the universal variable table.
    pub fn get_table(&self) -> &VarTable {
        &self.vars
    }

    /// Initialize this uvars for the default path.
    /// This should be called at most once on any given instance.
    pub fn initialize(&mut self) -> Option<CallbackDataList> {
        self.initialize_at_path(default_vars_path())
    }

    /// Initialize a this uvars for a given path.
    fn initialize_at_path(&mut self, path: WString) -> Option<CallbackDataList> {
        if path.is_empty() {
            return None;
        }
        assert!(!self.initialized(), "Already initialized");
        self.vars_path = path;

        self.load_from_path()
    }

    /// Reads and writes variables at the correct path.
    /// If there is an existing variables file with an unknown format, parsing it is attempted,
    /// but it will not be overwritten.
    /// Returns whether data was read, and the callbacks.
    pub fn sync(&mut self) -> (bool, Option<CallbackDataList>) {
        if !self.initialized() {
            return (false, None);
        }

        flog!(uvar_file, "universal log sync");
        // If we have no changes, just load.
        if self.modified.is_empty() {
            let callbacks = self.load_from_path_narrow();
            flog!(uvar_file, "universal log no modifications");
            return (false, callbacks);
        }

        flog!(uvar_file, "universal log performing full sync");

        let rewrite = |old_file: &File,
                       tmp_file: &mut File|
         -> std::io::Result<PotentialUpdate<Option<UniversalReadUpdate>>> {
            match self.load_from_file(old_file, file_id_for_file(old_file)?) {
                Some(potential_update) => {
                    if potential_update.do_save {
                        let contents = Self::serialize_with_vars(&potential_update.data.new_vars);
                        tmp_file.write_all(&contents)?;
                    }
                    Ok(PotentialUpdate {
                        do_save: potential_update.do_save,
                        data: Some(potential_update.data),
                    })
                }
                None => {
                    if self.ok_to_save {
                        let contents = Self::serialize_with_vars(&self.vars);
                        tmp_file.write_all(&contents)?;
                    }
                    Ok(PotentialUpdate {
                        do_save: self.ok_to_save,
                        data: None,
                    })
                }
            }
        };

        let real_path = wrealpath(&self.vars_path).unwrap_or_else(|_| self.vars_path.clone());
        match rewrite_via_temporary_file(&real_path, rewrite) {
            Ok((file_id, potential_update)) => {
                self.last_read_file_id = Some(file_id);
                self.ok_to_save = potential_update.do_save;
                self.modified.clear();
                match potential_update.data {
                    Some(UniversalReadUpdate {
                        export_generation_increment,
                        new_vars,
                        callbacks,
                        ok_to_save,
                    }) => {
                        assert_eq!(potential_update.do_save, ok_to_save);
                        self.export_generation += export_generation_increment;
                        self.vars = new_vars;
                        (true, Some(callbacks))
                    }
                    None => (true, None),
                }
            }
            Err(e) => {
                flog!(uvar_file, "universal log sync failed:", e);
                (false, None)
            }
        }
    }

    fn populate_variables(s: &[u8], out_vars: &mut VarTable) -> UvarFormat {
        let mut parsed = crate::universal_variables::Table::new();
        let format =
            crate::universal_variables::Codec::populate_variables(s, &mut parsed, |message| {
                flogf!(
                    warning,
                    "Unable to parse universal variable message: '%s'",
                    message
                );
            });
        out_vars.extend(parsed.into_iter().map(|(name, entry)| {
            (
                name,
                EnvVar::new_vec(
                    entry.values,
                    EnvVarFlags {
                        exported: entry.flags.exported,
                        pathvar: entry.flags.pathvar,
                        ..Default::default()
                    },
                ),
            )
        }));
        format
    }

    fn serialize_with_vars(vars: &VarTable) -> Vec<u8> {
        let vars = vars
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    crate::universal_variables::Entry {
                        values: value.as_list().to_vec(),
                        flags: crate::universal_variables::Flags {
                            exported: value.exports(),
                            pathvar: value.is_pathvar(),
                        },
                    },
                )
            })
            .collect();
        crate::universal_variables::Codec::serialize_with_vars(&vars, |name| {
            flogf!(error, "Illegal variable name: '%s'", name);
        })
    }

    #[cfg(test)]
    pub fn is_ok_to_save(&self) -> bool {
        self.ok_to_save
    }

    /// Access the export generation.
    pub fn get_export_generation(&self) -> u64 {
        self.export_generation
    }

    /// Return whether we are initialized.
    fn initialized(&self) -> bool {
        !self.vars_path.is_empty()
    }

    fn load_from_path(&mut self) -> Option<CallbackDataList> {
        self.narrow_vars_path = wcs2zstring(&self.vars_path);
        self.load_from_path_narrow()
    }

    fn load_from_path_narrow(&mut self) -> Option<CallbackDataList> {
        // Check to see if the file is unchanged. We do this again in load_from_file, but this avoids
        // opening the file unnecessarily.
        if self.last_read_file_id.is_some() {
            match file_id_for_path_narrow(&self.narrow_vars_path) {
                Ok(file_id) => {
                    if Some(file_id) == self.last_read_file_id {
                        flog!(uvar_file, "universal log sync elided based on fast stat()");
                        return None;
                    }
                }
                Err(e) => {
                    flog!(uvar_file, "failed to stat() universal variable file:", e);
                    return None;
                }
            }
        }

        flog!(uvar_file, "universal log reading from file");
        match lock_and_load(&self.vars_path, |f, file_id| {
            Ok(self.load_from_file(f, file_id).map(|update| update.data))
        }) {
            Ok((
                file_id,
                Some(UniversalReadUpdate {
                    export_generation_increment,
                    new_vars,
                    callbacks,
                    ok_to_save,
                }),
            )) => {
                self.export_generation += export_generation_increment;
                self.vars = new_vars;
                self.ok_to_save = ok_to_save;
                self.last_read_file_id = Some(file_id);
                Some(callbacks)
            }
            Ok((_, None)) => {
                // WARNING: Do not update self.file_id here, as this would interfere with
                // self.ok_to_save.
                None
            }
            Err(e) => {
                flog!(uvar_file, "Failed to load from universal variable file:", e);
                None
            }
        }
    }

    /// Load environment variables from the [`file`](`File`).
    /// If the file has not been modified since the last read, [`None`] is returned.
    /// In this case, `self.ok_to_save` needs to be consulted to decide whether to rewrite the file.
    /// Otherwise, the [`Some`] variant contains updated data and indicates whether the file should
    /// be rewritten..
    // IMPORTANT: Callers of this code assume that a return value of None means that the file id has
    // not changed. Do not return None in other situations without modifying the callers
    // accordingly. Otherwise, problems with self.ok_to_save are expected to occur.
    fn load_from_file(
        &self,
        file: &File,
        current_file_id: FileId,
    ) -> Option<PotentialUpdate<UniversalReadUpdate>> {
        if Some(current_file_id) == self.last_read_file_id {
            flog!(uvar_file, "universal log sync elided based on fstat()");
            return None;
        }
        // Read a variables table from the file.
        let mut new_vars = VarTable::new();
        let format = Self::read_message_internal(file, &mut new_vars);

        // Hacky: if the read format is in the future, avoid overwriting the file: never try to
        // save.
        let do_save = format != UvarFormat::Future;

        // Announce changes and update our exports generation.
        let (export_generation_increment, callbacks) =
            self.generate_callbacks_and_update_exports(&new_vars);

        // Acquire the new variables.
        self.acquire_variables(&mut new_vars);
        Some(PotentialUpdate {
            do_save,
            data: UniversalReadUpdate {
                export_generation_increment,
                new_vars,
                callbacks,
                ok_to_save: do_save,
            },
        })
    }

    /// Given a variable table, generate callbacks representing the difference between our vars and
    /// the new vars.
    /// Returns by how much the exports generation count should be incremented, as well as a
    /// callback list.
    fn generate_callbacks_and_update_exports(
        &self,
        new_vars: &VarTable,
    ) -> (u64, CallbackDataList) {
        let mut export_generation_increment = 0;
        let mut callbacks = CallbackDataList::new();
        // Construct callbacks for erased values.
        for (key, value) in &self.vars {
            // Skip modified values.
            if self.modified.contains(key) {
                continue;
            }

            // If the value is not present in new_vars, it has been erased.
            if !new_vars.contains_key(key) {
                callbacks.push(CallbackData {
                    key: key.clone(),
                    val: None,
                });
                if value.exports() {
                    export_generation_increment += 1;
                }
            }
        }

        // Construct callbacks for newly inserted or changed values.
        for (key, new_entry) in new_vars {
            // Skip modified values.
            if self.modified.contains(key) {
                continue;
            }

            let existing = self.vars.get(key);

            // See if the value has changed.
            let old_exports = existing.is_some_and(|v| v.exports());
            let export_changed = old_exports != new_entry.exports();
            let value_changed = existing.is_some_and(|v| v != new_entry);
            if export_changed || value_changed {
                export_generation_increment += 1;
            }
            if existing.is_none() || export_changed || value_changed {
                // Value is set for the first time, or has changed.
                callbacks.push(CallbackData {
                    key: key.clone(),
                    val: Some(new_entry.clone()),
                });
            }
        }
        (export_generation_increment, callbacks)
    }

    /// Copy modified values from existing vars to `vars_to_acquire`.
    fn acquire_variables(&self, vars_to_acquire: &mut VarTable) {
        for key in &self.modified {
            match self.vars.get(key) {
                None => {
                    /* The value has been deleted. */
                    vars_to_acquire.remove(key);
                }
                Some(src) => {
                    // The value has been modified. Copy it over.
                    vars_to_acquire.insert(key.clone(), src.clone());
                }
            }
        }
    }

    fn read_message_internal(file: &File, vars: &mut VarTable) -> UvarFormat {
        let mut contents = vec![];
        // Read everything from the file. Put a sane limit on it.
        // TODO: Ideally, the cast should be checked at compile time.
        if let Err(e) = file
            .take(u64::try_from(MAX_READ_SIZE).expect("MAX_READ_SIZE must fit into u64"))
            .read_to_end(&mut contents)
        {
            flog!(warning, "Failed to read file:", e);
        }

        // Handle overlong files.
        // We read at most `MAX_READ_SIZE` bytes due to the `take`.
        if contents.len() == MAX_READ_SIZE {
            // Back up to a newline.
            let newline = contents.iter().rposition(|c| *c == b'\n').unwrap_or(0);
            contents.truncate(newline);
        }

        Self::populate_variables(&contents, vars)
    }
}

/// Return the default variable path, or an empty string on failure.
pub fn default_vars_path() -> WString {
    let ValidatedPath { path, ok } = path_get_config();
    if !ok {
        return WString::new();
    }
    let mut path = path.to_owned();
    path.push_str("/fish_variables");
    path
}

#[cfg(test)]
mod tests {
    use crate::{
        env::{EnvVar, EnvVarFlags},
        env_universal_common::EnvUniversal,
        prelude::*,
        tests::prelude::*,
        wutil::file_id_for_path,
    };
    use fish_tempfile::TempDir;
    use fish_widestring::{osstr2wcstring, wcs2osstring};

    const UVARS_PER_THREAD: usize = 8;

    /// Creates a unique temporary directory and file path for universal variable tests inside the
    /// new tempdir.
    /// Returns (temp_dir, file_path).
    fn make_test_uvar_path() -> std::io::Result<(TempDir, WString)> {
        let temp_dir = fish_tempfile::new_dir()?;
        let file_path = temp_dir.path().join("varsfile.txt");
        let file_path = osstr2wcstring(file_path);
        Ok((temp_dir, file_path))
    }

    fn test_universal_helper(x: usize, path: &wstr) {
        test_init();
        let mut uvars = EnvUniversal::new();
        uvars.initialize_at_path(path.to_owned());

        for j in 0..UVARS_PER_THREAD {
            let key = sprintf!("key_%d_%d", x, j);
            let val = sprintf!("val_%d_%d", x, j);
            uvars.set(&key, EnvVar::new(val, EnvVarFlags::default()));
            let (synced, _) = uvars.sync();
            assert!(
                synced,
                "Failed to sync universal variables after modification"
            );
        }

        // Last step is to delete the first key.
        uvars.remove(&sprintf!("key_%d_%d", x, 0));
        let (synced, _) = uvars.sync();
        assert!(synced, "Failed to sync universal variables after deletion");
    }

    #[test]
    fn test_universal() {
        test_init();
        let (_test_dir, test_path) = make_test_uvar_path().unwrap();

        let threads = 1;
        let mut handles = Vec::new();

        for i in 0..threads {
            let path = test_path.clone();
            handles.push(std::thread::spawn(move || {
                test_universal_helper(i, &path);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let mut uvars = EnvUniversal::new();
        uvars.initialize_at_path(test_path);

        for i in 0..threads {
            for j in 0..UVARS_PER_THREAD {
                let key = sprintf!("key_%d_%d", i, j);
                let expected_val = if j == 0 {
                    None
                } else {
                    Some(EnvVar::new(
                        sprintf!("val_%d_%d", i, j),
                        EnvVarFlags::default(),
                    ))
                };
                let var = uvars.get(&key);
                assert_eq!(var, expected_val);
            }
        }
    }

    #[test]
    fn test_universal_callbacks() {
        test_init();
        let (_test_dir, test_path) = make_test_uvar_path().unwrap();
        let mut uvars1 = EnvUniversal::new();
        let mut uvars2 = EnvUniversal::new();
        let mut callbacks = uvars1
            .initialize_at_path(test_path.clone())
            .unwrap_or_default();
        callbacks.append(&mut uvars2.initialize_at_path(test_path).unwrap_or_default());

        macro_rules! sync {
            ($uvars:expr) => {
                let (_, cb_opt) = $uvars.sync();
                if let Some(mut cb) = cb_opt {
                    callbacks.append(&mut cb);
                }
            };
        }

        let noflags = EnvVarFlags::default();

        // Put some variables into both.
        uvars1.set(L!("alpha"), EnvVar::new(L!("1").to_owned(), noflags)); //
        uvars1.set(L!("beta"), EnvVar::new(L!("1").to_owned(), noflags)); //
        uvars1.set(L!("delta"), EnvVar::new(L!("1").to_owned(), noflags)); //
        uvars1.set(L!("epsilon"), EnvVar::new(L!("1").to_owned(), noflags)); //
        uvars1.set(L!("lambda"), EnvVar::new(L!("1").to_owned(), noflags)); //
        uvars1.set(L!("kappa"), EnvVar::new(L!("1").to_owned(), noflags)); //
        uvars1.set(L!("omicron"), EnvVar::new(L!("1").to_owned(), noflags)); //

        sync!(uvars1);
        sync!(uvars2);

        // Change uvars1.
        uvars1.set(L!("alpha"), EnvVar::new(L!("2").to_owned(), noflags)); // changes value
        uvars1.set(
            L!("beta"),
            EnvVar::new(
                L!("1").to_owned(),
                EnvVarFlags {
                    exported: true,
                    ..Default::default()
                },
            ),
        ); // changes export
        uvars1.remove(L!("delta")); // erases value
        uvars1.set(L!("epsilon"), EnvVar::new(L!("1").to_owned(), noflags)); // changes nothing
        sync!(uvars1);

        // Change uvars2. It should treat its value as correct and ignore changes from uvars1.
        uvars2.set(L!("lambda"), EnvVar::new(L!("1").to_owned(), noflags)); // same value
        uvars2.set(L!("kappa"), EnvVar::new(L!("2").to_owned(), noflags)); // different value

        // Now see what uvars2 sees.
        callbacks.clear();
        sync!(uvars2);

        // Sort them to get them in a predictable order.
        callbacks.sort_by(|a, b| a.key.cmp(&b.key));

        // Should see exactly three changes.
        assert_eq!(callbacks.len(), 3);
        assert_eq!(callbacks[0].key, L!("alpha"));
        assert_eq!(callbacks[0].val.as_ref().unwrap().as_string(), L!("2"));
        assert_eq!(callbacks[1].key, L!("beta"));
        assert_eq!(callbacks[1].val.as_ref().unwrap().as_string(), L!("1"));
        assert_eq!(callbacks[2].key, L!("delta"));
        assert_eq!(callbacks[2].val, None);
    }

    #[test]
    fn test_universal_ok_to_save() {
        test_init();
        // Ensure we don't try to save after reading from a newer fish.
        let (_test_dir, test_path) = make_test_uvar_path().unwrap();
        let contents = b"# VERSION: 99999.99\n";
        std::fs::write(wcs2osstring(&test_path), contents).unwrap();

        let before_id = file_id_for_path(&test_path).unwrap();

        let mut uvars = EnvUniversal::new();
        uvars
            .initialize_at_path(test_path.clone())
            .unwrap_or_default();
        assert!(!uvars.is_ok_to_save(), "Should not be OK to save");
        uvars.sync();
        assert!(!uvars.is_ok_to_save(), "Should still not be OK to save");
        uvars.set(
            L!("SOMEVAR"),
            EnvVar::new(L!("SOMEVALUE").to_owned(), EnvVarFlags::default()),
        );

        // Ensure file is same.
        let after_id = file_id_for_path(&test_path).unwrap();
        assert_eq!(before_id, after_id, "test_path should not have changed",);
    }
}
