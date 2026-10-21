// Copyright (C) 1989-2006 Free Software Foundation, Inc.
// Rust translation: Copyright (C) 2026 fish-shell contributors.
// Derived from GNU Bash 3.2.57 print_cmd.c. GPL version 2 or later; see ../COPYING.

use super::{Command, CommandKind, List, LogicalOp, Pipeline, Redirect, RedirectKind, Word};

/// Bash's multiline `declare -f` representation, retaining original word quoting.
pub fn format_function(name: &[u8], body: &Command) -> Vec<u8> {
    let mut printer = Printer(Vec::new());
    printer.function(name, body, 0);
    printer.put(b"\n");
    printer.0
}

struct Printer(Vec<u8>);

impl Printer {
    fn put(&mut self, text: &[u8]) {
        self.0.extend_from_slice(text);
    }
    fn indent(&mut self, depth: usize) {
        self.0.extend(std::iter::repeat_n(b' ', depth));
    }
    fn newline(&mut self, depth: usize) {
        self.put(b"\n");
        self.indent(depth);
    }
    fn semicolon(&mut self) {
        if !matches!(self.0.last(), Some(b'&' | b'\n')) {
            self.put(b";");
        }
    }
    fn words(&mut self, words: &[Word], separator: &[u8]) {
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                self.put(separator);
            }
            self.put(&word.raw);
        }
    }

    fn function(&mut self, name: &[u8], body: &Command, depth: usize) {
        self.put(name);
        self.put(b" () ");
        self.newline(depth);
        self.put(b"{ \n");
        if let CommandKind::Group(list) = &body.kind {
            self.list(list, depth + 4, false);
        } else {
            self.indent(depth + 4);
            self.command(body, depth + 4);
        }
        self.newline(depth);
        self.put(b"}");
        if matches!(body.kind, CommandKind::Group(_)) {
            self.redirects(&body.redirects);
        }
    }

    fn list(&mut self, list: &List, depth: usize, inline: bool) {
        for (i, item) in list.iter().enumerate() {
            if i == 0 {
                if !inline {
                    self.indent(depth);
                }
            } else if list[i - 1].background {
                self.put(b" ");
            } else {
                self.semicolon();
                self.newline(depth);
            }
            self.pipeline(&item.chain.first, depth);
            for (op, pipeline) in &item.chain.rest {
                self.put(match op {
                    LogicalOp::And => b" && ",
                    LogicalOp::Or => b" || ",
                });
                self.pipeline(pipeline, depth);
            }
            if item.background {
                self.put(b" &");
            }
        }
    }

    fn pipeline(&mut self, pipeline: &Pipeline, depth: usize) {
        if pipeline.negate {
            self.put(b"! ");
        }
        for (i, command) in pipeline.commands.iter().enumerate() {
            if i > 0 {
                self.put(b" | ");
            }
            self.command(command, depth);
        }
    }

    fn command(&mut self, command: &Command, depth: usize) {
        match &command.kind {
            CommandKind::Simple(words) => self.words(words, b" "),
            CommandKind::Group(body) => {
                self.put(b"{ \n");
                self.list(body, depth + 4, false);
                self.newline(depth);
                self.put(b"}");
            }
            CommandKind::Subshell(body) => {
                self.put(b"( ");
                self.list(body, depth, true);
                self.put(b" )");
            }
            CommandKind::If {
                branches,
                else_body,
            } => self.if_command(branches, else_body, depth),
            CommandKind::For { name, words, body } => {
                self.put(b"for ");
                self.put(&name.raw);
                self.put(b" in ");
                if let Some(words) = words {
                    self.words(words, b" ");
                } else {
                    self.put(b"\"$@\"");
                }
                self.put(b";");
                self.newline(depth);
                self.put(b"do\n");
                self.list(body, depth + 4, false);
                self.semicolon();
                self.newline(depth);
                self.put(b"done");
            }
            CommandKind::While {
                until,
                condition,
                body,
            } => {
                self.put(if *until { b"until " } else { b"while " });
                self.list(condition, depth, true);
                self.semicolon();
                self.put(b" do\n");
                self.list(body, depth + 4, false);
                self.semicolon();
                self.newline(depth);
                self.put(b"done");
            }
            CommandKind::Case { word, arms } => {
                self.put(b"case ");
                self.put(&word.raw);
                self.put(b" in ");
                for arm in arms {
                    self.newline(depth + 4);
                    self.words(&arm.patterns, b" | ");
                    self.put(b")\n");
                    self.list(&arm.body, depth + 8, false);
                    self.newline(depth + 4);
                    self.put(b";;");
                }
                self.newline(depth);
                self.put(b"esac");
            }
            CommandKind::Function { name, body } => {
                self.put(b"function ");
                self.function(&name.raw, body, depth);
            }
            CommandKind::Arithmetic(word) => {
                self.put(b"((");
                self.put(&word.raw);
                self.put(b"))");
            }
            CommandKind::Conditional(words) => {
                self.put(b"[[ ");
                self.condition(words);
                self.put(b" ]]");
            }
        }
        self.redirects(&command.redirects);
    }

    fn if_command(&mut self, branches: &[(List, List)], else_body: &List, depth: usize) {
        let (condition, body) = &branches[0];
        self.put(b"if ");
        self.list(condition, depth, true);
        self.semicolon();
        self.put(b" then\n");
        self.list(body, depth + 4, false);
        self.semicolon();
        if branches.len() > 1 || !else_body.is_empty() {
            self.newline(depth);
            self.put(b"else\n");
            if branches.len() > 1 {
                self.indent(depth + 4);
                self.if_command(&branches[1..], else_body, depth + 4);
            } else {
                self.list(else_body, depth + 4, false);
            }
            self.semicolon();
        }
        self.newline(depth);
        self.put(b"fi");
    }

    fn condition(&mut self, words: &[Word]) {
        for operator in [b"||".as_slice(), b"&&"] {
            let mut depth = 0;
            for (i, word) in words.iter().enumerate() {
                match word.raw.as_slice() {
                    b"(" => depth += 1,
                    b")" => depth -= 1,
                    _ if depth == 0 && word.raw == operator => {
                        self.condition(&words[..i]);
                        self.put(b" ");
                        self.put(operator);
                        self.put(b" ");
                        self.condition(&words[i + 1..]);
                        return;
                    }
                    _ => {}
                }
            }
        }
        if words.first().is_some_and(|w| w.raw == b"!") {
            self.put(b"! ");
            self.condition(&words[1..]);
        } else if words.first().is_some_and(|w| w.raw == b"(")
            && words.last().is_some_and(|w| w.raw == b")")
        {
            self.put(b"( ");
            self.condition(&words[1..words.len() - 1]);
            self.put(b" )");
        } else {
            if words.len() == 1 {
                self.put(b"-n ");
            }
            self.words(words, b" ");
        }
    }

    fn redirects(&mut self, redirects: &[Redirect]) {
        if redirects.is_empty() {
            return;
        }
        self.put(b" ");
        for (i, redirect) in redirects.iter().enumerate() {
            if i > 0 {
                self.put(b" ");
            }
            if redirect.heredoc.is_none() {
                self.redirect(redirect);
            }
        }
        if redirects.iter().any(|r| r.heredoc.is_some()) {
            self.put(b" ");
            for redirect in redirects.iter().filter(|r| r.heredoc.is_some()) {
                self.redirect(redirect);
            }
        }
    }

    fn redirect(&mut self, redirect: &Redirect) {
        let input = matches!(
            redirect.kind,
            RedirectKind::Read
                | RedirectKind::ReadWrite
                | RedirectKind::DupRead
                | RedirectKind::HereDoc
                | RedirectKind::HereDocStrip
                | RedirectKind::HereString
        );
        let fd = redirect.fd.unwrap_or(i32::from(!input));
        let output_file = redirect.kind == RedirectKind::DupWrite
            && fd == 1
            && !redirect
                .target
                .raw
                .strip_suffix(b"-")
                .unwrap_or(&redirect.target.raw)
                .iter()
                .all(u8::is_ascii_digit);
        let always_fd = matches!(
            redirect.kind,
            RedirectKind::DupRead | RedirectKind::DupWrite
        );
        let printed_default = if redirect.kind == RedirectKind::ReadWrite {
            1
        } else {
            i32::from(!input)
        };
        if !output_file && (always_fd || fd != printed_default) {
            self.put(fd.to_string().as_bytes());
        }
        self.put(match redirect.kind {
            RedirectKind::Read => b"< ",
            RedirectKind::Write => b"> ",
            RedirectKind::Append => b">> ",
            RedirectKind::Clobber => b">|",
            RedirectKind::ReadWrite => b"<> ",
            RedirectKind::DupRead if redirect.target.raw == b"-" => b">&",
            RedirectKind::DupRead => b"<&",
            RedirectKind::DupWrite => b">&",
            RedirectKind::HereDoc => b"<<",
            RedirectKind::HereDocStrip => b"<<-",
            RedirectKind::HereString => b"<<< ",
            RedirectKind::Both => b">&",
        });
        if let Some(doc) = &redirect.heredoc {
            let (delimiter, expand) =
                super::heredoc_delimiter(&redirect.target).expect("parsed delimiter");
            if expand {
                self.put(&delimiter);
            } else {
                self.put(b"'");
                for &byte in &delimiter {
                    if byte == b'\'' {
                        self.put(b"'\\''");
                    } else {
                        self.0.push(byte);
                    }
                }
                self.put(b"'");
            }
            self.put(b"\n");
            self.put(&doc.body);
            self.put(&delimiter);
            self.put(b"\n");
        } else {
            self.put(&redirect.target.raw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_definition_has_bash_spacing_and_word_quoting() {
        let parsed = super::super::parse_one(b"f() { printf 'value:%s\\n' \"$x\"; }").unwrap();
        let CommandKind::Function { name, body } = &parsed.body[0].chain.first.commands[0].kind
        else {
            panic!("function");
        };
        assert_eq!(
            format_function(&name.raw, body),
            b"f () \n{ \n    printf 'value:%s\\n' \"$x\"\n}\n"
        );
    }
}
