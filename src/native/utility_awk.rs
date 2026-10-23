//! Command-line adapter for the shared awk-rs interpreter.
//!
//! The interpreter owns AWK syntax and evaluation; this module only supplies
//! command-line options, input files and the child's standard streams.

use awk_rs::{Interpreter, Lexer, Parser, SourceLocation, Value, ast::Expr};
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read as _, Write as _};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Options {
    source: String,
    assignments: Vec<(String, String)>,
    files: Vec<String>,
    posix: bool,
    traditional: bool,
}

// AWK can exit in BEGIN or while reading an earlier file. Opening inputs only
// when the interpreter reads them preserves that ordering and its side effects.
struct InputFile {
    path: String,
    reader: Option<BufReader<std::fs::File>>,
}

impl InputFile {
    fn reader(&mut self) -> io::Result<&mut BufReader<std::fs::File>> {
        if self.reader.is_none() {
            self.reader = Some(BufReader::new(std::fs::File::open(&self.path)?));
        }
        Ok(self.reader.as_mut().unwrap())
    }
}

impl io::Read for InputFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reader()?.read(buffer)
    }
}

impl BufRead for InputFile {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.reader()?.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        if let Some(reader) = self.reader.as_mut() {
            reader.consume(amount);
        }
    }
}

pub(super) fn run(args: Vec<OsString>) -> i32 {
    let result: Result<i32> = (|| {
        let args = args
            .into_iter()
            .skip(1)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| "arguments must be valid UTF-8")
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if args.first().is_some_and(|arg| arg == "--version") {
            writeln!(io::stdout(), "awk (awk-rs) 0.2.0")?;
            return Ok(0);
        }
        if args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "--help" | "-h"))
        {
            writeln!(
                io::stdout(),
                "Usage: awk [-F separator] [-v name=value] [-f program-file] [--] [program] [file ...]\nPackaged awk-rs interpreter. AWK command pipes and system() require sh."
            )?;
            return Ok(0);
        }
        let options = parse(&args)?;
        let tokens = Lexer::new(&options.source).tokenize()?;
        let program = Parser::new(tokens).parse()?;
        let mut interpreter = Interpreter::new(&program);
        interpreter.set_posix_mode(options.posix);
        interpreter.set_traditional_mode(options.traditional);
        let mut argv = vec!["awk".to_owned()];
        argv.extend(options.files.iter().cloned());
        interpreter.set_args(argv);
        for (name, value) in options.assignments {
            // set_variable() in awk-rs 0.2 bypasses special-variable setters.
            // Its public lvalue API handles FS, OFS, ORS and ordinary variables.
            interpreter.assign_to_lvalue(
                &Expr::Var(name, SourceLocation::new(1, 1)),
                Value::from_string(value),
            )?;
        }
        let mut inputs: Vec<Box<dyn BufRead>> = Vec::new();
        let mut filenames = Vec::new();
        if options.files.is_empty() {
            inputs.push(Box::new(BufReader::new(io::stdin())));
            filenames.push(String::new());
        } else {
            for name in options.files {
                if name == "-" {
                    inputs.push(Box::new(BufReader::new(io::stdin())));
                } else {
                    inputs.push(Box::new(InputFile {
                        path: name.clone(),
                        reader: None,
                    }));
                }
                filenames.push(name);
            }
        }
        interpreter.set_filenames(filenames);
        Ok(interpreter.run(inputs, &mut io::stdout().lock())?)
    })();
    match result {
        Ok(status) => status,
        Err(error) => {
            let _ = writeln!(io::stderr(), "awk: {error}");
            2
        }
    }
}

fn parse(args: &[String]) -> Result<Options> {
    let mut options = Options {
        source: String::new(),
        assignments: Vec::new(),
        files: Vec::new(),
        posix: false,
        traditional: false,
    };
    let mut source_files = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--" => {
                index += 1;
                break;
            }
            "--posix" | "-P" => {
                options.posix = true;
                options.traditional = false;
            }
            "--traditional" | "--compat" | "-c" => {
                options.traditional = true;
                options.posix = false;
            }
            _ if arg.starts_with("-F") || arg.starts_with("-v") || arg.starts_with("-f") => {
                let flag = &arg[..2];
                let value = if arg.len() == 2 {
                    index += 1;
                    args.get(index)
                        .ok_or_else(|| format!("{flag} requires an argument"))?
                } else {
                    &arg[2..]
                };
                match flag {
                    "-F" => options.assignments.push(("FS".to_owned(), unescape(value))),
                    "-v" => {
                        let (name, value) = value.split_once('=').ok_or("-v expects name=value")?;
                        if !valid_name(name) {
                            return Err(format!("invalid variable name: {name}").into());
                        }
                        options.assignments.push((name.to_owned(), unescape(value)));
                    }
                    "-f" => {
                        let source = if value == "-" {
                            let mut source = String::new();
                            io::stdin().read_to_string(&mut source)?;
                            source
                        } else {
                            std::fs::read_to_string(value)?
                        };
                        options.source.push_str(&source);
                        options.source.push('\n');
                        source_files = true;
                    }
                    _ => unreachable!(),
                }
            }
            _ if arg.starts_with('-') && arg != "-" => {
                return Err(format!("unknown option: {arg}").into());
            }
            _ => break,
        }
        index += 1;
    }
    if !source_files {
        options
            .source
            .clone_from(args.get(index).ok_or("no program provided")?);
        index += 1;
    }
    for operand in &args[index..] {
        if operand
            .split_once('=')
            .is_some_and(|(name, _)| valid_name(name))
        {
            return Err("assignment operands between input files are not supported by packaged awk; use -v name=value".into());
        }
        options.files.push(operand.clone());
    }
    Ok(options)
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn unescape(value: &str) -> String {
    let mut chars = value.chars().peekable();
    let mut result = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        let Some(c) = chars.next() else {
            result.push('\\');
            break;
        };
        match c {
            'a' => result.push('\u{7}'),
            'b' => result.push('\u{8}'),
            'f' => result.push('\u{c}'),
            'n' => result.push('\n'),
            'r' => result.push('\r'),
            't' => result.push('\t'),
            'v' => result.push('\u{b}'),
            '0'..='7' => {
                let mut value = c.to_digit(8).unwrap();
                for _ in 0..2 {
                    let Some(next) = chars.peek().and_then(|c| c.to_digit(8)) else {
                        break;
                    };
                    chars.next();
                    value = value * 8 + next;
                }
                result.push(char::from((value & 0xff) as u8));
            }
            'x' => {
                let mut value = 0;
                let mut digits = 0;
                for _ in 0..2 {
                    let Some(next) = chars.peek().and_then(|c| c.to_digit(16)) else {
                        break;
                    };
                    chars.next();
                    value = value * 16 + next;
                    digits += 1;
                }
                result.push(if digits == 0 {
                    'x'
                } else {
                    char::from(value as u8)
                });
            }
            _ => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_assignments_reach_the_interpreter() {
        let source = r#"{ printf "%s|%s\n", $1, $2 }"#;
        let program = Parser::new(Lexer::new(source).tokenize().unwrap())
            .parse()
            .unwrap();
        let mut interpreter = Interpreter::new(&program);
        interpreter
            .assign_to_lvalue(
                &Expr::Var("FS".to_owned(), SourceLocation::new(1, 1)),
                Value::from_string(" +- +".to_owned()),
            )
            .unwrap();
        let mut output = Vec::new();
        interpreter
            .run(
                vec![BufReader::new("command - description\n".as_bytes())],
                &mut output,
            )
            .unwrap();
        assert_eq!(output, b"command|description\n");
    }

    #[test]
    fn assignment_escapes_match_awk_strings() {
        assert_eq!(unescape(r"a\tb\n\141\x62\\"), "a\tb\nab\\");
    }
}
