//! Variable-aware directory lookup with a separate, unevaluated insertion text.
use super::*;

pub(super) struct VariablePath {
    raw: String,
    raw_directory: String,
    display_directory: String,
    directory_quote: Option<char>,
    directories: Vec<String>,
    prefix: String,
}

impl CompletionEngine {
    pub(super) fn variable_path(
        &self,
        line: &str,
        pos: usize,
        word: &Word,
        option: &str,
    ) -> Option<VariablePath> {
        if !self.fish || self.input_language != InputLanguage::Fish {
            return None;
        }
        let raw = line.get(word.start..pos)?;
        let argument = raw.strip_prefix(option)?;
        let split = argument.rfind('/')? + 1;
        let (directory, basename) = argument.split_at(split);
        let directory_quote = completion_word(directory, directory.len(), true).quote;
        let closed_directory = close_quote(directory, directory_quote);
        let directories =
            super::super::fish::completion_words(&closed_directory, &self.variable_values, true)
                .ok()?;
        if directories.is_empty() {
            return None;
        }
        // Supplying the initial quote context lets the same parser unescape
        // only the basename. An empty literal prevents a basename's leading ~
        // from acquiring home-expansion semantics after the directory slash.
        let initial = directory_quote.map_or_else(|| "''".into(), |quote| quote.to_string());
        let closed_basename = close_quote(&format!("{initial}{basename}"), word.quote);
        let prefixes =
            super::super::fish::completion_words(&closed_basename, &self.variable_values, false)
                .ok()?;
        let [prefix] = prefixes.as_slice() else {
            return None;
        };
        if prefix.contains(['/', '\\', '\n', '\r']) {
            return None;
        }
        let display_split = word.value.rfind('/')? + 1;
        Some(VariablePath {
            raw: raw.into(),
            raw_directory: format!("{option}{directory}"),
            display_directory: word.value[..display_split].into(),
            directory_quote,
            directories,
            prefix: prefix.clone(),
        })
    }

    pub(super) fn complete_variable_paths(
        &self,
        word: &Word,
        path: &VariablePath,
        line: &str,
        pos: usize,
        cwd: &Path,
        matches: &mut Candidates,
    ) {
        let tail = token_tail(line, pos, word.quote);
        for directory in &path.directories {
            let directory = directory.replace('\\', "/");
            let mut expanded = word.clone();
            expanded.value = format!("{directory}{}", path.prefix);
            let mut paths = Candidates::new(&expanded.value);
            self.complete_paths(&expanded, cwd, &mut paths);
            for entry in paths.entries.into_values() {
                let Some(name) = entry.value.strip_prefix(&directory) else {
                    continue;
                };
                let (mut replacement, quote) = if let Some(suffix) = name.strip_prefix(&path.prefix)
                {
                    (
                        format!("{}{}", path.raw, quote_fragment(suffix, word.quote)),
                        word.quote,
                    )
                } else {
                    (
                        format!(
                            "{}{}",
                            path.raw_directory,
                            quote_fragment(name, path.directory_quote)
                        ),
                        path.directory_quote,
                    )
                };
                if let Some(quote) = quote {
                    // Files finish an unfinished quote at end-of-line. Keep
                    // directory quotes open so the next component continues
                    // in the same context; preserve an existing closing quote.
                    if (entry.space && pos == line.len()) || tail.ends_with(quote) {
                        replacement.push(quote);
                    }
                }
                let display = format!("{}{name}", path.display_directory);
                matches.add_ranked(
                    &display,
                    &display,
                    &entry.description,
                    entry.space,
                    entry.rank,
                );
                if let Some(candidate) = matches.entries.get_mut(&display) {
                    candidate.replacement = Some(replacement);
                }
            }
        }
    }
}

fn close_quote(value: &str, quote: Option<char>) -> String {
    let mut value = value.to_owned();
    if let Some(quote) = quote {
        value.push(quote);
    }
    value
}

fn quote_fragment(value: &str, quote: Option<char>) -> String {
    let Some(quote) = quote else {
        return quote_completion(value);
    };
    let mut escaped = String::new();
    for character in value.chars() {
        if character.is_control() {
            escaped.push(quote);
            escaped.push_str(&format!("\\u{:04x}", character as u32));
            escaped.push(quote);
            continue;
        }
        if character == quote || character == '\\' || quote == '"' && character == '$' {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}
