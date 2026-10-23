//! Native file/console frontend for the shared Fish formatter.
use super::interactive_colors::Palette;
use super::interactive_highlight::{Role, highlight_roles};
use crate::indent::{apply_indents, compute_indents, unindent};
use fish_wgetopt::{ArgType, WGetopter, wopt};
use fish_widestring::{
    WString, bytes2wcstring, osstr2wcstring, prelude::*, wcs2bytes, wcs2osstring,
};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::{self, Read as _, Write as _},
};

const HELP: &str = "fish_indent - format Fish source\n\nUsage: fish_indent [OPTIONS] [FILE ...]\n\nRead standard input when no files are given.\n  -w, --write          Rewrite each file in place\n  -c, --check          Report files that need formatting\n  -i, --no-indent      Format without indentation\n      --only-indent    Add indentation without reformatting\n      --only-unindent  Remove indentation without reformatting\n      --ansi           Emit native syntax colors\n      --html           Emit highlighted HTML\n      --pygments       Emit start,end,role CSV for the original source\n      --dump-parse-tree  Write the syntax tree and metrics to stderr\n  -v, --version        Print version\n  -h, --help           Print this help\n";

#[derive(Clone, Copy, PartialEq)]
enum Output {
    Plain,
    Write,
    Check,
    Ansi,
    Html,
    Pygments,
}

pub fn main() -> i32 {
    match run() {
        Ok(status) => status,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "fish_indent: {error}");
            1
        }
    }
}

fn run() -> io::Result<i32> {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    if let Ok(features) = std::env::var("fish_features") {
        fish_feature_flags::set_from_string(WString::from(features).as_utfstr());
    }
    let arguments: Vec<WString> = std::env::args_os().map(osstr2wcstring).collect();
    let mut args: Vec<&wstr> = arguments.iter().map(|arg| arg.as_ref()).collect();
    let options = [
        wopt(L!("dump-parse-tree"), ArgType::NoArgument, 'P'),
        wopt(L!("no-indent"), ArgType::NoArgument, 'i'),
        wopt(L!("only-indent"), ArgType::NoArgument, '\x04'),
        wopt(L!("only-unindent"), ArgType::NoArgument, '\x05'),
        wopt(L!("help"), ArgType::NoArgument, 'h'),
        wopt(L!("version"), ArgType::NoArgument, 'v'),
        wopt(L!("write"), ArgType::NoArgument, 'w'),
        wopt(L!("html"), ArgType::NoArgument, '\x01'),
        wopt(L!("ansi"), ArgType::NoArgument, '\x02'),
        wopt(L!("pygments"), ArgType::NoArgument, '\x03'),
        wopt(L!("check"), ArgType::NoArgument, 'c'),
    ];
    let mut parser = WGetopter::new(L!("+hvwic"), &options, &mut args);
    let mut output = Output::Plain;
    let mut do_indent = true;
    let mut only_indent = false;
    let mut only_unindent = false;
    let mut dump = false;
    while let Some(option) = parser.next_opt() {
        match option {
            'h' => {
                write!(stdout, "{HELP}")?;
                stdout.flush()?;
                return Ok(0);
            }
            'v' => {
                writeln!(stdout, "fish_indent, version {}", crate::BUILD_VERSION)?;
                stdout.flush()?;
                return Ok(0);
            }
            'P' => dump = true,
            'w' => output = Output::Write,
            'c' => output = Output::Check,
            'i' => do_indent = false,
            '\x01' => output = Output::Html,
            '\x02' => output = Output::Ansi,
            '\x03' => output = Output::Pygments,
            '\x04' => only_indent = true,
            '\x05' => only_unindent = true,
            _ => {
                let reason = if option == ';' {
                    "option does not take an argument"
                } else {
                    "unknown option"
                };
                writeln!(
                    stderr,
                    "fish_indent: {}: {reason}",
                    parser.argv[parser.wopt_index - 1]
                )?;
                return Ok(1);
            }
        }
    }
    let files = &parser.argv[parser.wopt_index..];
    if files.is_empty() && output == Output::Write {
        writeln!(
            stderr,
            "fish_indent: expected file path to read/write for -w"
        )?;
        return Ok(1);
    }
    let variables = std::env::vars()
        .filter(|(name, _)| name.starts_with("fish_color_"))
        .map(|(name, value)| (name, value.split_whitespace().map(str::to_owned).collect()))
        .collect::<BTreeMap<_, _>>();
    let palette = Palette::from_variables(&variables);
    let mut status = 0i32;
    for index in 0..files.len().max(1) {
        let mut input = vec![];
        if files.is_empty() {
            io::stdin().read_to_end(&mut input)?;
        } else {
            input = fs::read(wcs2osstring(files[index]))?;
        }
        let source = bytes2wcstring(&input);
        if output == Output::Pygments {
            stdout.write_all(render(&source, output, &palette).as_bytes())?;
            continue;
        }
        if dump {
            write!(stderr, "{}", crate::formatting::dump_parse_tree(&source))?;
        }
        let formatted = if only_indent || only_unindent {
            let indents = compute_indents(&source);
            if only_indent {
                apply_indents(&source, &indents)
            } else {
                unindent(&source, &indents)
            }
        } else {
            crate::formatting::prettify(&source, do_indent)
        };
        match output {
            Output::Check if source != formatted => {
                if let Some(file) = files.get(index) {
                    writeln!(stderr, "{file}")?;
                }
                status = status.saturating_add(1);
            }
            Output::Check => {}
            Output::Write if source != formatted => {
                fs::write(wcs2osstring(files[index]), wcs2bytes(&formatted))?;
            }
            Output::Write => {}
            Output::Plain => stdout.write_all(&wcs2bytes(&formatted))?,
            Output::Ansi | Output::Html => {
                stdout.write_all(render(&formatted, output, &palette).as_bytes())?;
            }
            Output::Pygments => unreachable!(),
        }
    }
    stdout.flush()?;
    Ok(status)
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Normal => "normal",
        Role::Command | Role::Keyword => "command",
        Role::Quote => "quote",
        Role::Escape => "escape",
        Role::Variable | Role::Operator => "operat",
        Role::Comment => "comment",
        Role::Error => "error",
        Role::Param => "param",
        Role::Option => "option",
        Role::Redirection => "redirection",
        Role::End => "statement_terminator",
    }
}

fn render(source: &wstr, output: Output, palette: &Palette) -> String {
    let roles = highlight_roles(source);
    let mut result = String::new();
    if output == Output::Html {
        result.push_str("<pre><code>");
    }
    let mut start = 0;
    while start < roles.len() {
        let role = roles[start];
        let end = start
            + roles[start..]
                .iter()
                .take_while(|&&next| next == role)
                .count();
        if output == Output::Pygments {
            writeln!(result, "{start},{end},{}", role_name(role)).unwrap();
        } else {
            if output == Output::Html {
                write!(result, "<span class=\"{}\">", role.variable()).unwrap();
            } else {
                result.push_str(&palette.sequence(role.variable(), false));
            }
            for character in source[start..end].chars() {
                if output == Output::Html {
                    match character {
                        '&' => result.push_str("&amp;"),
                        '<' => result.push_str("&lt;"),
                        '>' => result.push_str("&gt;"),
                        '\'' => result.push_str("&apos;"),
                        '"' => result.push_str("&quot;"),
                        _ => result.push(character),
                    }
                } else {
                    result.push(character);
                }
            }
            if output == Output::Html {
                result.push_str("</span>");
            }
        }
        start = end;
    }
    if output == Output::Html {
        result.push_str("</code></pre>");
    }
    if output == Output::Ansi {
        result.push_str("\x1b[0m");
    }
    result
}
