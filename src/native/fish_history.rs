//! Native adapter for Fish history. Search matching and serialization are
//! shared with POSIX; storage and evaluator/editor ownership are Windows-specific.
use super::*;
use crate::history_core::{HistoryItem, SearchType, time_to_seconds};
use crate::native::history::History;
use std::collections::HashSet;
use std::io::IsTerminal as _;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Command {
    #[default]
    Search,
    Delete,
    Clear,
    ClearSession,
    Merge,
    Save,
    Append,
}

impl Command {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "search" => Self::Search,
            "delete" => Self::Delete,
            "clear" => Self::Clear,
            "clear-session" => Self::ClearSession,
            "merge" => Self::Merge,
            "save" => Self::Save,
            "append" => Self::Append,
            _ => return None,
        })
    }
}

#[derive(Default)]
struct Options {
    command: Option<Command>,
    search_type: Option<SearchType>,
    time_format: Option<String>,
    max: Option<usize>,
    case_sensitive: bool,
    null: bool,
    reverse: bool,
    color: Option<bool>,
    help: bool,
    terms: Vec<String>,
}

impl Options {
    fn command(&mut self, command: Command) -> Result<()> {
        if self.command.replace(command).is_some() {
            return Err(Error::new("history: subcommands are mutually exclusive"));
        }
        Ok(())
    }

    fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg == "--" {
                options.terms.extend(args.cloned());
                break;
            }
            if let Some(long) = arg.strip_prefix("--") {
                let (name, inline) = long
                    .split_once('=')
                    .map_or((long, None), |(n, v)| (n, Some(v)));
                if inline.is_some() && !matches!(name, "show-time" | "max" | "color") {
                    return Err(Error::new(format!("history: --{name} takes no argument")));
                }
                match name {
                    "search" | "delete" | "clear" | "merge" | "save" => {
                        options.command(Command::parse(name).unwrap())?;
                    }
                    "prefix" => options.search_type = Some(SearchType::PrefixGlob),
                    "contains" => options.search_type = Some(SearchType::ContainsGlob),
                    "exact" => options.search_type = Some(SearchType::Exact),
                    "case-sensitive" => options.case_sensitive = true,
                    "null" => options.null = true,
                    "reverse" => options.reverse = true,
                    "help" => options.help = true,
                    "show-time" => options.time_format = Some(inline.unwrap_or("# %c%n").into()),
                    "max" | "color" => {
                        let value = inline
                            .or_else(|| args.next().map(String::as_str))
                            .ok_or_else(|| {
                                Error::new(format!("history: --{name} requires an argument"))
                            })?;
                        if name == "max" {
                            options.max = Some(parse_max(value)?);
                        } else {
                            options.color = match value {
                                "always" => Some(true),
                                "never" => Some(false),
                                "auto" => None,
                                _ => {
                                    return Err(Error::new(
                                        "history: --color expects always, never or auto",
                                    ));
                                }
                            };
                        }
                    }
                    _ => return Err(Error::new(format!("history: unknown option --{name}"))),
                }
            } else if let Some(short) = arg.strip_prefix('-').filter(|s| !s.is_empty()) {
                if short.bytes().all(|c| c.is_ascii_digit()) {
                    options.max = Some(parse_max(short)?);
                    continue;
                }
                for (index, letter) in short.char_indices() {
                    match letter {
                        'p' => options.search_type = Some(SearchType::PrefixGlob),
                        'c' => options.search_type = Some(SearchType::ContainsGlob),
                        'e' => options.search_type = Some(SearchType::Exact),
                        'C' => options.case_sensitive = true,
                        'z' => options.null = true,
                        'R' => options.reverse = true,
                        'h' => options.help = true,
                        'm' => options.command(Command::Merge)?,
                        'n' | 't' => {
                            let rest = &short[index + letter.len_utf8()..];
                            let value = if !rest.is_empty() {
                                rest
                            } else if letter == 't' {
                                "# %c%n"
                            } else {
                                args.next()
                                    .map(String::as_str)
                                    .ok_or_else(|| Error::new("history: -n requires an argument"))?
                            };
                            if letter == 'n' {
                                options.max = Some(parse_max(value)?);
                            } else {
                                options.time_format = Some(value.into());
                            }
                            break;
                        }
                        _ => return Err(Error::new(format!("history: unknown option -{letter}"))),
                    }
                }
            } else {
                options.terms.push(arg.clone());
            }
        }
        if let Some(command) = options.terms.first().and_then(|s| Command::parse(s)) {
            options.command(command)?;
            options.terms.remove(0);
        }
        Ok(options)
    }

    fn check_no_arguments(&self) -> Result<()> {
        if self.search_type.is_some()
            || self.time_format.is_some()
            || self.null
            || !self.terms.is_empty()
        {
            return Err(Error::new(
                "history: subcommand takes no search options or arguments",
            ));
        }
        Ok(())
    }
}

fn parse_max(value: &str) -> Result<usize> {
    // Keep the POSIX builtin's signed-number conversion (negative means all).
    value
        .parse::<i64>()
        .map(|n| n as usize)
        .map_err(|_| Error::new(format!("history: {value}: not a number")))
}

impl Session {
    fn history_path(&self) -> Option<PathBuf> {
        if self.private_history {
            return None;
        }
        let name = self
            .variable("fish_history")
            .map_or_else(|| "fish".into(), |v| v.values.join(" "));
        if name.is_empty() {
            return None;
        }
        let name = if name.chars().all(|c| c == '_' || c.is_alphanumeric()) {
            name.as_str()
        } else {
            "fish"
        };
        let absolute = |key| {
            self.values(key)
                .first()
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let directory = absolute("XDG_DATA_HOME")
            .or_else(|| absolute("LOCALAPPDATA"))
            .or_else(|| {
                absolute("HOME")
                    .or_else(|| absolute("USERPROFILE"))
                    .map(|p| p.join(".local/share"))
            })?;
        Some(
            directory
                .join("fish")
                .join(format!("native_{name}_history.yaml")),
        )
    }

    fn native_history(&mut self) -> Result<&mut History> {
        let path = self.history_path();
        if self
            .history
            .as_ref()
            .is_none_or(|history| history.path() != path.as_deref())
        {
            if let Some(history) = &mut self.history {
                history.finish_command()?;
            }
            self.history = Some(History::new(path));
        }
        Ok(self.history.as_mut().unwrap())
    }

    pub(in crate::native) fn history_snapshot(&mut self) -> Result<Vec<String>> {
        Ok(self.native_history()?.snapshot()?)
    }

    pub(in crate::native) fn begin_history(&mut self, source: &str) -> Result<()> {
        self.native_history()?
            .begin_command(source.trim_end_matches(['\r', '\n']))?;
        Ok(())
    }

    pub(in crate::native) fn finish_history(&mut self) -> Result<()> {
        if let Some(history) = &mut self.history {
            history.finish_command()?;
        }
        Ok(())
    }

    pub(super) fn history_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let options = Options::parse(args)?;
        if options.help {
            self.write(b"history [search] [-p|-c|-e] [-C] [-n N] [-R] [-z] [-t[FORMAT]] [TERM ...]\nhistory append COMMAND ...\nhistory delete --exact --case-sensitive COMMAND ...\nhistory save|merge|clear|clear-session\nSearch also accepts --color=auto|always|never.\n", host, output)?;
            self.status = 0;
            return Ok(());
        }
        match options.command.unwrap_or_default() {
            Command::Search => {
                if options.terms.iter().any(String::is_empty) {
                    return Err(Error {
                        message: "history: searching for the empty string isn't allowed".into(),
                        status: 1,
                    });
                }
                let items = self.native_history()?.items()?;
                let selected = select(&items, &options);
                let color = options.color.unwrap_or_else(|| {
                    !output.capture
                        && output.redirects.is_empty()
                        && std::io::stdout().is_terminal()
                });
                let palette = crate::native::interactive_colors::Palette::from_variables(
                    &self.editor_variables(),
                );
                let resolver = color.then(|| {
                    let mut engine =
                        crate::native::interactive_completion::CompletionEngine::new("fish");
                    let (commands, path, variables) = self.completion_context();
                    engine.set_context(commands, path, variables);
                    engine
                });
                let cwd = std::env::current_dir()?;
                for item in selected {
                    if let Some(format) = &options.time_format {
                        let text = format_time(item, format);
                        self.write(text.as_bytes(), host, output)?;
                    }
                    let command = item.str().to_string();
                    let command = if color {
                        crate::native::interactive_highlight::highlight_for_language(
                            &command,
                            command.len(),
                            |name| resolver.as_ref().unwrap().command_exists(name, &cwd),
                            &palette,
                            self.input_language,
                        )
                    } else {
                        command
                    };
                    self.write(command.as_bytes(), host, output)?;
                    self.write(if options.null { b"\0" } else { b"\n" }, host, output)?;
                }
            }
            Command::Append => {
                let history = self.native_history()?;
                for term in &options.terms {
                    history.append(term)?;
                }
                history.save()?;
            }
            Command::Delete => {
                if options.search_type.unwrap_or(SearchType::Exact) != SearchType::Exact
                    || !options.case_sensitive
                {
                    return Err(Error::new(
                        "history: builtin delete requires --exact --case-sensitive",
                    ));
                }
                self.native_history()?.delete(&options.terms)?;
            }
            Command::Clear | Command::ClearSession => {
                options.check_no_arguments()?;
                let history = self.native_history()?;
                if options.command == Some(Command::Clear) {
                    history.clear()?;
                } else {
                    history.clear_session()?;
                }
                history.save()?;
            }
            Command::Save => {
                options.check_no_arguments()?;
                self.native_history()?.save()?;
            }
            Command::Merge => {
                options.check_no_arguments()?;
                if self.private_history {
                    return Err(Error::new("history: can't merge history in private mode"));
                }
                self.native_history()?.merge()?;
            }
        }
        self.status = 0;
        Ok(())
    }
}

fn select<'a>(items: &'a [HistoryItem], options: &Options) -> Vec<&'a HistoryItem> {
    let mut result = Vec::new();
    let default_term = vec![String::new()];
    let terms = if options.terms.is_empty() {
        &default_term
    } else {
        &options.terms
    };
    for term in terms {
        let mut seen = HashSet::new();
        let term = WString::from(term.as_str());
        let term = if options.case_sensitive {
            term
        } else {
            term.to_lowercase()
        };
        for item in items {
            if result.len() == options.max.unwrap_or(usize::MAX) {
                break;
            }
            if item.matches_search(
                &term,
                if options.terms.is_empty() {
                    SearchType::Contains
                } else {
                    options.search_type.unwrap_or(SearchType::ContainsGlob)
                },
                options.case_sensitive,
            ) && seen.insert(item.str().to_owned())
            {
                result.push(item);
            }
        }
    }
    if options.reverse {
        result.reverse();
    }
    result
}

fn format_time(item: &HistoryItem, format: &str) -> String {
    jiff::Timestamp::from_second(time_to_seconds(item.last_added_timestamp()))
        .ok()
        .and_then(|time| {
            jiff::fmt::strtime::format(format, &time.to_zoned(jiff::tz::TimeZone::system())).ok()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_core::{PersistenceMode, Timestamps};
    use std::time::UNIX_EPOCH;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }
    fn items(values: &[&str]) -> Vec<HistoryItem> {
        values
            .iter()
            .map(|s| {
                HistoryItem::new(
                    WString::from(*s),
                    Timestamps {
                        first_added: UNIX_EPOCH,
                        last_added: UNIX_EPOCH,
                    },
                    PersistenceMode::Disk,
                )
            })
            .collect()
    }
    fn selected(values: &[&str], arguments: &[&str]) -> Vec<String> {
        select(&items(values), &Options::parse(&args(arguments)).unwrap())
            .iter()
            .map(|item| item.str().to_string())
            .collect()
    }

    #[test]
    fn options_permute_preserve_terms_and_reject_conflicting_subcommands() {
        let options =
            Options::parse(&args(&["search", "echo", "-Cez", "-n2", "--reverse"])).unwrap();
        assert_eq!(options.command, Some(Command::Search));
        assert_eq!(options.terms, ["echo"]);
        assert_eq!(options.search_type, Some(SearchType::Exact));
        assert!(options.case_sensitive && options.null && options.reverse);
        assert_eq!(options.max, Some(2));
        assert!(Options::parse(&args(&["--clear", "save"])).is_err());
        assert!(Options::parse(&args(&["--max", "bad"])).is_err());
        assert!(Options::parse(&args(&["--reverse=yes"])).is_err());
        assert_eq!(Options::parse(&args(&["--", "-C"])).unwrap().terms, ["-C"]);
        assert_eq!(
            Options::parse(&args(&["-t%Y"]))
                .unwrap()
                .time_format
                .as_deref(),
            Some("%Y")
        );
    }

    #[test]
    fn search_order_per_term_dedup_and_limit_before_reverse_match_posix() {
        let input = [
            "echo New",
            "git status",
            "echo old",
            "echo New",
            "printf 🐟",
        ];
        assert_eq!(selected(&input, &["-n2", "-R"]), ["git status", "echo New"]);
        assert_eq!(
            selected(&input, &["search", "-e", "ECHO NEW"]),
            ["echo New"]
        );
        assert!(selected(&input, &["-Ce", "ECHO NEW"]).is_empty());
        assert_eq!(selected(&input, &["-p", "git"]), ["git status"]);
        assert_eq!(
            selected(&input, &["echo", "New"]),
            ["echo New", "echo old", "echo New"]
        );
        assert!(selected(&input, &["--max=0"]).is_empty());
        assert_eq!(selected(&input, &["--exact", "-n1"]), ["echo New"]);
    }
}
