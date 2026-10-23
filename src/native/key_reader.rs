//! Standalone native key reader sharing the interactive editor's input decoder.
use super::interactive_bindings::key_name;
use rustyline::{KeyCode, KeyEvent, Modifiers, error::ReadlineError, key_reader::RawKeyReader};
use std::{
    collections::VecDeque,
    io::{self, IsTerminal as _, Write as _},
    time::Instant,
};

const HELP: &str = "fish_key_reader - show Fish key binding names\n\nUsage: fish_key_reader [OPTIONS]\n  -c, --continuous  Keep reading until exit, quit, or two ctrl-c/ctrl-d keys\n  -V, --verbose     Show decoded key details and elapsed time\n  -v, --version     Print version\n  -h, --help        Print this help\n";

pub fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("fish_key_reader: {error}");
            1
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut continuous = false;
    let mut verbose = false;
    let mut options = true;
    for argument in std::env::args().skip(1) {
        if !options {
            return Err(format!("unexpected argument: {argument}").into());
        }
        match argument.as_str() {
            "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "--version" => {
                println!("fish_key_reader, version {}", crate::BUILD_VERSION);
                return Ok(());
            }
            "--continuous" => continuous = true,
            "--verbose" => verbose = true,
            "--" => options = false,
            arg if arg.len() > 1 && arg.starts_with('-') && !arg.starts_with("--") => {
                for flag in arg[1..].chars() {
                    match flag {
                        'c' => continuous = true,
                        'V' => verbose = true,
                        'h' => {
                            print!("{HELP}");
                            return Ok(());
                        }
                        'v' => {
                            println!("fish_key_reader, version {}", crate::BUILD_VERSION);
                            return Ok(());
                        }
                        _ => return Err(format!("unknown option: {argument}").into()),
                    }
                }
            }
            _ => return Err(format!("unexpected argument: {argument}").into()),
        }
    }
    if !io::stdin().is_terminal() {
        return Err("standard input must be a console".into());
    }
    let mut reader = RawKeyReader::new()?;
    if continuous {
        eprintln!("Type exit or quit, or press ctrl-c or ctrl-d twice, to exit.");
    }
    eprintln!("Press a key:");
    let mut recent = Recent::default();
    let mut previous = Instant::now();
    loop {
        let key = match reader.read_key() {
            Ok(key) => key,
            Err(ReadlineError::WindowResized) => continue,
            Err(ReadlineError::Eof) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if verbose {
            println!(
                "# decoded key: {key:?}; elapsed: {:.3} ms",
                previous.elapsed().as_secs_f64() * 1000.0
            );
        }
        previous = Instant::now();
        if let Some(text) = reader.read_paste(key)? {
            println!("# bracketed paste: {}", text.escape_debug());
        } else if let Some(name) = key_name(key) {
            println!("bind {} 'do something'", quote_key(&name));
        } else {
            println!("# Key is not supported by native bind: {key:?}");
        }
        io::stdout().flush()?;
        if !continuous || recent.should_exit(key) {
            break;
        }
    }
    Ok(())
}

fn quote_key(name: &str) -> String {
    if name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
    {
        name.to_owned()
    } else {
        format!("'{}'", name.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

#[derive(Default)]
struct Recent(VecDeque<KeyEvent>);
impl Recent {
    fn should_exit(&mut self, key: KeyEvent) -> bool {
        let key = KeyEvent::normalize(key);
        let repeated_control = matches!(key.0, KeyCode::Char('C' | 'D'))
            && key.1 == Modifiers::CTRL
            && self.0.back() == Some(&key);
        if self.0.len() == 4 {
            self.0.pop_front();
        }
        self.0.push_back(key);
        repeated_control
            || ["exit", "quit"].iter().any(|word| {
                self.0.len() == 4
                    && self
                        .0
                        .iter()
                        .zip(word.chars())
                        .all(|(key, ch)| *key == KeyEvent::from(ch))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_consecutive_exit_keys() {
        let mut recent = Recent::default();
        assert!(!recent.should_exit(KeyEvent::ctrl('c')));
        assert!(recent.should_exit(KeyEvent::ctrl('c')));
        let mut recent = Recent::default();
        for ch in "exi".chars() {
            assert!(!recent.should_exit(KeyEvent::from(ch)));
        }
        assert!(recent.should_exit(KeyEvent::from('t')));
        let mut recent = Recent::default();
        for ch in "exit".chars() {
            assert!(!recent.should_exit(KeyEvent::alt(ch)));
        }
        assert!(!recent.should_exit(KeyEvent::ctrl('d')));
        assert!(!recent.should_exit(KeyEvent::from('x')));
        assert!(!recent.should_exit(KeyEvent::ctrl('d')));
    }

    #[test]
    fn binding_names_are_safe_fish_arguments() {
        assert_eq!(quote_key("ctrl-x"), "ctrl-x");
        assert_eq!(quote_key("'"), "'\\''");
        assert_eq!(quote_key("\\"), "'\\\\'");
        assert_eq!(
            key_name(KeyEvent(KeyCode::Left, Modifiers::CTRL)).as_deref(),
            Some("ctrl-left")
        );
        assert_eq!(
            key_name(KeyEvent(KeyCode::BackTab, Modifiers::SHIFT)).as_deref(),
            Some("shift-tab")
        );
    }
}
