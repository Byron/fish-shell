//! Native startup reads the user's portable Fish configuration. POSIX system
//! initialization (locale, terminal setup, login paths and system snippets)
//! belongs to the POSIX frontend.

use std::path::PathBuf;

pub(super) fn files() -> Vec<PathBuf> {
    let Some(directory) = super::interactive::config_directory("fish") else {
        return Vec::new();
    };
    let mut snippets: Vec<_> = std::fs::read_dir(directory.join("conf.d"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "fish")
        })
        .filter(|path| path.is_file())
        .collect();
    snippets.sort();
    if let Some(config) = super::interactive::startup_file("fish") {
        snippets.push(config);
    }
    snippets
}
