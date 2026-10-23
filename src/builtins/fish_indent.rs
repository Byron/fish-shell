//! The fish_indent program.

use super::prelude::*;
use crate::{
    builtins::Error,
    common::{PROGRAM_NAME, get_program_name},
    env::{EnvStack, Environment as _, env_init},
    err_fmt, err_str,
    global_safety::RelaxedAtomicBool,
    highlight::{HighlightRole, HighlightSpec, colorize, highlight_shell},
    locale::set_libc_locales,
    operation_context::OperationContext,
    panic::panic_handler,
    parse_util::{apply_indents, compute_indents},
    prelude::*,
    print_help::print_help,
    threads, topic_monitor,
};
use fish_common::ReadExt as _;
use fish_wgetopt::{ArgType, WGetopter, WOption, wopt};
use fish_widestring::{bytes2wcstring, osstr2wcstring, wcs2bytes};
use std::{
    ffi::OsStr,
    fmt::Write as _,
    fs,
    io::{Read, Write as _},
    os::unix::ffi::OsStrExt as _,
};

pub fn main() {
    PROGRAM_NAME.set(L!("fish_indent")).unwrap();
    panic_handler(throwing_main)
}

fn throwing_main() -> i32 {
    // TODO: Duplicated with fish_key_reader
    use crate::fds::BorrowedFdFile;
    use crate::io::{FdOutputStream, IoChain, OutputStream::Fd};
    use libc::{STDERR_FILENO, STDOUT_FILENO};

    topic_monitor::init();
    threads::init();

    let mut out = Fd(FdOutputStream::new(STDOUT_FILENO));
    let mut err = Fd(FdOutputStream::new(STDERR_FILENO));
    let io_chain = IoChain::new();
    let mut streams = IoStreams::new(&mut out, &mut err, &io_chain);
    streams.stdin_file = Some(BorrowedFdFile::stdin());
    // Safety: single-threaded.
    unsafe {
        set_libc_locales(/*log_ok=*/ false)
    };
    #[cfg(feature = "localize-messages")]
    crate::localization::initialize_localization();
    env_init(None, false);

    // Only set these here so you can't set them via the builtin.
    if let Some(features_var) = EnvStack::globals().get(L!("fish_features")) {
        for s in features_var.as_list() {
            fish_feature_flags::set_from_string(s.as_utfstr());
        }
    }

    let args: Vec<WString> = std::env::args_os().map(osstr2wcstring).collect();
    do_indent(None, &mut streams, args).builtin_status_code()
}

pub fn fish_indent(
    parser: &mut Parser,
    streams: &mut IoStreams,
    args: &mut [&wstr],
) -> BuiltinResult {
    let args = args.iter_mut().map(|x| x.to_owned()).collect();
    do_indent(Some(parser), streams, args)
}

fn do_indent(
    parser: Option<&mut Parser>,
    streams: &mut IoStreams,
    args: Vec<WString>,
) -> BuiltinResult {
    // Types of output we support
    #[derive(PartialEq)]
    enum OutputType {
        PlainText,
        File,
        Ansi,
        PygmentsCsv,
        Check,
        Html,
    }

    let mut output_type = OutputType::PlainText;
    let mut output_location = L!("");
    let mut do_indent = true;
    let mut only_indent = false;
    let mut only_unindent = false;

    let short_opts: &wstr = L!("+hvwic");
    let long_opts: &[WOption] = &[
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

    let mut shim_args: Vec<&wstr> = args.iter().map(|s| s.as_ref()).collect();
    let mut w = WGetopter::new(short_opts, long_opts, &mut shim_args);

    while let Some(c) = w.next_opt() {
        match c {
            'P' => DUMP_PARSE_TREE.store(true),
            'h' => {
                if let Some(parser) = parser {
                    builtin_print_help(parser, streams, L!("fish_indent"));
                } else {
                    print_help("fish_indent");
                }
                return Ok(SUCCESS);
            }
            'v' => {
                streams
                    .out
                    .appendln(&localized_version_string(get_program_name()));
                return Ok(SUCCESS);
            }
            'w' => output_type = OutputType::File,
            'i' => do_indent = false,
            '\x04' => only_indent = true,
            '\x05' => only_unindent = true,
            '\x01' => output_type = OutputType::Html,
            '\x02' => output_type = OutputType::Ansi,
            '\x03' => output_type = OutputType::PygmentsCsv,
            'c' => output_type = OutputType::Check,
            ';' => {
                err_fmt!(Error::UNEXP_OPT_ARG, w.argv[w.wopt_index - 1])
                    .cmd(L!("fish_indent"))
                    .finish(streams);
                return Err(STATUS_CMD_ERROR);
            }
            '?' => {
                err_fmt!(Error::UNKNOWN_OPT, w.argv[w.wopt_index - 1])
                    .cmd(L!("fish_indent"))
                    .finish(streams);
                return Err(STATUS_CMD_ERROR);
            }
            _ => panic!(),
        }
    }

    let args = &w.argv[w.wopt_index..];

    let mut retval = 0;

    let mut src;
    let mut i = 0;
    while i < args.len() || (args.is_empty() && i == 0) {
        if args.is_empty() && i == 0 {
            if output_type == OutputType::File {
                err_str!("Expected file path to read/write for -w:")
                    .append_to_msg(&sprintf!("\n\n $ %s -w foo.fish\n", get_program_name()))
                    .finish(streams);
                return Err(STATUS_CMD_ERROR);
            }
            let Some(stdin_file) = streams.stdin_file.as_mut() else {
                let cmd = L!("fish_indent");
                err_str!(Error::STDIN_CLOSED).cmd(cmd).finish(streams);
                return Err(STATUS_CMD_ERROR);
            };
            let mut buf = vec![];
            match stdin_file.read_to_end_interruptible(&mut buf) {
                Ok(_) => {}
                Err(err) => {
                    return if err.kind() == std::io::ErrorKind::Interrupted {
                        Err(128 + libc::SIGINT)
                    } else {
                        Err(STATUS_CMD_ERROR)
                    };
                }
            }
            src = bytes2wcstring(&buf);
        } else {
            let arg = args[i];
            match fs::File::open(OsStr::from_bytes(&wcs2bytes(arg))) {
                Ok(file) => {
                    match read_file(file) {
                        Ok(s) => src = s,
                        Err(()) => return Err(STATUS_CMD_ERROR),
                    }
                    output_location = arg;
                }
                Err(err) => {
                    err_fmt!("Opening \"%s\" failed: %s", arg, err.to_string()).finish(streams);
                    return Err(STATUS_CMD_ERROR);
                }
            }
        }

        if output_type == OutputType::PygmentsCsv {
            let output = make_pygments_csv(&src);
            streams.out.append(&bytes2wcstring(&output));
            i += 1;
            continue;
        }

        let output_wtext = if only_indent || only_unindent {
            let indents = compute_indents(&src);
            if only_indent {
                apply_indents(&src, &indents)
            } else {
                crate::indent::unindent(&src, &indents)
            }
        } else {
            prettify(streams, &src, do_indent)
        };

        // Maybe colorize.
        let mut colors = vec![];
        if output_type != OutputType::PlainText {
            highlight_shell(
                &output_wtext,
                &mut colors,
                &mut OperationContext::globals(),
                false,
                None,
            );
        }

        let mut colored_output = vec![];
        match output_type {
            OutputType::PlainText => {
                colored_output = no_colorize(&output_wtext);
            }
            OutputType::File => {
                if output_wtext != src {
                    match fs::File::create(OsStr::from_bytes(&wcs2bytes(output_location))) {
                        Ok(mut file) => {
                            _ = file.write_all(&wcs2bytes(&output_wtext));
                        }
                        Err(err) => {
                            err_fmt!(
                                "Opening \"%s\" failed: %s",
                                output_location,
                                err.to_string()
                            )
                            .finish(streams);
                            return Err(STATUS_CMD_ERROR);
                        }
                    }
                }
            }
            OutputType::Ansi => {
                colored_output = colorize(&output_wtext, &colors, EnvStack::globals());
            }
            OutputType::Html => {
                colored_output = html_colorize(&output_wtext, &colors);
            }
            OutputType::PygmentsCsv => {
                unreachable!()
            }
            OutputType::Check => {
                if output_wtext != src {
                    if let Some(arg) = args.get(i) {
                        streams.err.appendln(*arg);
                    }
                    retval += 1;
                }
            }
        }

        streams.out.append(&bytes2wcstring(&colored_output));
        i += 1;
    }
    if retval == 0 {
        Ok(SUCCESS)
    } else {
        Err(retval)
    }
}

static DUMP_PARSE_TREE: RelaxedAtomicBool = RelaxedAtomicBool::new(false);

// Read the entire contents of a file into the specified string.
fn read_file(mut f: impl Read) -> Result<WString, ()> {
    let mut buf = vec![];
    f.read_to_end(&mut buf).map_err(|_| ())?;
    Ok(bytes2wcstring(&buf))
}

// Entry point for Pygments CSV output.
// Our output is a newline-separated string.
// Each line is of the form `start,end,role`
// start and end is the half-open token range, value is a string from highlight_role_t.
// Example:
// 3,7,command
fn make_pygments_csv(src: &wstr) -> Vec<u8> {
    let mut colors = vec![];
    highlight_shell(
        src,
        &mut colors,
        &mut OperationContext::globals(),
        false,
        None,
    );
    assert_eq!(
        colors.len(),
        src.len(),
        "Colors and src should have same size"
    );

    struct TokenRange {
        start: usize,
        end: usize,
        role: HighlightRole,
    }

    let mut token_ranges: Vec<TokenRange> = vec![];
    for (i, color) in colors.iter().copied().enumerate() {
        let role = color.foreground;
        // See if we can extend the last range.
        if let Some(last) = token_ranges.last_mut()
            && last.role == role
            && last.end == i
        {
            last.end = i + 1;
            continue;
        }
        // We need a new range.
        token_ranges.push(TokenRange {
            start: i,
            end: i + 1,
            role,
        });
    }

    // Now render these to a string.
    let mut result = String::with_capacity(token_ranges.len() * 32);
    for range in token_ranges {
        writeln!(result, "{},{},{}", range.start, range.end, range.role).unwrap();
    }
    result.into_bytes()
}

// Entry point for prettification.
fn prettify(streams: &mut IoStreams, src: &wstr, do_indent: bool) -> WString {
    if DUMP_PARSE_TREE.load() {
        streams
            .err
            .appendln(&crate::formatting::dump_parse_tree(src));
    }
    crate::formatting::prettify(src, do_indent)
}

/// Given a string and list of colors of the same size, return the string with HTML span elements
/// for the various colors.
fn html_class_name_for_color(spec: HighlightSpec) -> &'static wstr {
    match spec.foreground {
        HighlightRole::Normal => L!("fish_color_normal"),
        HighlightRole::Error => L!("fish_color_error"),
        HighlightRole::Command => L!("fish_color_command"),
        HighlightRole::Builtin => L!("fish_color_builtin"),
        HighlightRole::Function => L!("fish_color_function"),
        HighlightRole::StatementTerminator => L!("fish_color_statement_terminator"),
        HighlightRole::Param => L!("fish_color_param"),
        HighlightRole::Option => L!("fish_color_option"),
        HighlightRole::Comment => L!("fish_color_comment"),
        HighlightRole::SearchMatch => L!("fish_color_search_match"),
        HighlightRole::Operat => L!("fish_color_operator"),
        HighlightRole::Escape => L!("fish_color_escape"),
        HighlightRole::Quote => L!("fish_color_quote"),
        HighlightRole::Redirection => L!("fish_color_redirection"),
        HighlightRole::Autosuggestion => L!("fish_color_autosuggestion"),
        HighlightRole::Selection => L!("fish_color_selection"),
        _ => L!("fish_color_other"),
    }
}

fn html_colorize(text: &wstr, colors: &[HighlightSpec]) -> Vec<u8> {
    if text.is_empty() {
        return vec![];
    }

    assert_eq!(colors.len(), text.len());
    let mut html = L!("<pre><code>").to_owned();
    let mut last_color = HighlightSpec::new();
    for (i, (wc, &color)) in text.chars().zip(colors).enumerate() {
        // Handle colors.
        if i > 0 && color != last_color {
            html.push_str("</span>");
        }
        if i == 0 || color != last_color {
            sprintf!(=> &mut html, "<span class=\"%s\">", html_class_name_for_color(color));
        }
        last_color = color;

        // Handle text.
        match wc {
            '&' => html.push_str("&amp;"),
            '\'' => html.push_str("&apos;"),
            '"' => html.push_str("&quot;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            _ => html.push(wc),
        }
    }
    html.push_str("</span></code></pre>");
    wcs2bytes(&html)
}

fn no_colorize(text: &wstr) -> Vec<u8> {
    wcs2bytes(text)
}
