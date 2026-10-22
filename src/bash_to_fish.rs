//! Bash-shaped interactive syntax with Fish expansion, scope and command resolution.
//! This is deliberately separate from both Rash evaluation and completion projection.

use crate::prelude::*;
use fish_widestring::{bytes2wcstring, wcs2bytes};
use rash::syntax::{self, Command, CommandKind, List, LogicalOp, Pipeline, RedirectKind};

#[cfg(not(windows))]
use crate::bash_syntax::literal_backticks;

/// Native input uses the same literal-backtick dialect without depending on
/// the POSIX reader's tree-sitter, highlighting and session adapters.
#[cfg(windows)]
pub(crate) fn literal_backticks(source: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if source.contains(&b'`') {
        std::borrow::Cow::Owned(
            source
                .iter()
                .map(|&byte| if byte == b'`' { b'^' } else { byte })
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(source)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    pub incomplete: bool,
}

impl Error {
    fn unsupported(feature: &str) -> Self {
        Self {
            message: format!("{feature} is not supported in Bash-style Fish input"),
            incomplete: false,
        }
    }
}

impl From<syntax::ParseError> for Error {
    fn from(error: syntax::ParseError) -> Self {
        Self {
            incomplete: error.is_incomplete(),
            message: error.message,
        }
    }
}

/// Translate and validate the entire submission before the caller executes any of it.
/// Backticks are literal; other lexical quoting follows Bash, with Fish evaluation.
pub fn translate(source: &wstr) -> Result<WString, Error> {
    if source.contains('\0') {
        return Err(Error::unsupported("NUL input"));
    }
    let result = bytes2wcstring(&translate_bytes(&wcs2bytes(source), 0)?);
    #[cfg(not(windows))]
    {
        let mut errors = Vec::new();
        if crate::parse_util::detect_parse_errors(&result, Some(&mut errors), false).is_err() {
            return Err(Error {
                message: errors.first().map_or_else(
                    || "translated input is not valid Fish syntax".to_owned(),
                    |error| error.text.to_string(),
                ),
                incomplete: false,
            });
        }
    }
    #[cfg(windows)]
    crate::native::fish::check_syntax(&result.to_string()).map_err(|error| Error {
        message: error.message,
        incomplete: false,
    })?;
    Ok(result)
}

fn translate_bytes(source: &[u8], depth: usize) -> Result<Vec<u8>, Error> {
    if depth >= 128 {
        return Err(Error::unsupported("nesting beyond 128 levels"));
    }
    let mut offset = 0;
    let input = literal_backticks(source);
    let mut output = Printer(Vec::new(), depth, source);
    while offset < source.len() {
        let unit = syntax::parse_one(&input[offset..])?;
        if unit.consumed == 0 {
            break;
        }
        output.2 = &source[offset..];
        output.list(&unit.body)?;
        offset += unit.consumed;
    }
    Ok(output.0)
}

struct Printer<'a>(Vec<u8>, usize, &'a [u8]);

impl<'a> Printer<'a> {
    fn raw(&self, word: &syntax::Word) -> &'a [u8] {
        &self.2[word.span.clone()]
    }

    fn put(&mut self, text: &[u8]) {
        self.0.extend_from_slice(text);
    }

    fn list(&mut self, list: &List) -> Result<(), Error> {
        for item in list {
            self.chain(&item.chain)?;
            if item.background {
                self.put(b" &");
            }
            self.put(b"\n");
        }
        Ok(())
    }

    fn chain(&mut self, chain: &syntax::AndOr) -> Result<(), Error> {
        self.pipeline(&chain.first)?;
        for (op, pipeline) in &chain.rest {
            self.put(match op {
                LogicalOp::And => b" && ",
                LogicalOp::Or => b" || ",
            });
            self.pipeline(pipeline)?;
        }
        Ok(())
    }

    fn pipeline(&mut self, pipeline: &Pipeline) -> Result<(), Error> {
        if pipeline.negate {
            self.put(b"not ");
        }
        for (index, command) in pipeline.commands.iter().enumerate() {
            if index != 0 {
                self.put(b" | ");
            }
            self.command(command)?;
        }
        Ok(())
    }

    fn condition(&mut self, condition: &List, negate: bool) -> Result<(), Error> {
        if condition.is_empty() {
            return Err(Error::unsupported("an empty condition"));
        }
        if let [item] = condition.as_slice() {
            if !item.background && (!negate || item.chain.rest.is_empty()) {
                self.chain(&item.chain)?;
                self.put(b"\n");
                return Ok(());
            }
        }
        self.put(b"begin\n");
        self.list(condition)?;
        self.put(b"end\n");
        Ok(())
    }

    fn command(&mut self, command: &Command) -> Result<(), Error> {
        if self.1 >= 128 {
            return Err(Error::unsupported("nesting beyond 128 levels"));
        }
        self.1 += 1;
        let result = self.command_inner(command);
        self.1 -= 1;
        result
    }

    fn command_inner(&mut self, command: &Command) -> Result<(), Error> {
        match &command.kind {
            CommandKind::Simple(words) => self.simple(words)?,
            CommandKind::Group(body) => {
                self.put(b"begin\n");
                self.list(body)?;
                self.put(b"end");
            }
            CommandKind::If {
                branches,
                else_body,
            } => {
                for (index, (condition, body)) in branches.iter().enumerate() {
                    self.put(if index == 0 { b"if " } else { b"else if " });
                    self.condition(condition, false)?;
                    self.list(body)?;
                }
                if !else_body.is_empty() {
                    self.put(b"else\n");
                    self.list(else_body)?;
                }
                self.put(b"end");
            }
            CommandKind::For { name, words, body } => {
                self.put(b"for ");
                self.put(self.raw(name));
                self.put(b" in");
                if let Some(words) = words {
                    for word in words {
                        self.put(b" ");
                        self.put(&translate_word(self.raw(word), self.1)?);
                    }
                } else {
                    self.put(b" $argv");
                }
                self.put(b"\n");
                self.list(body)?;
                self.put(b"end");
            }
            CommandKind::While {
                until,
                condition,
                body,
            } => {
                self.put(if *until { b"while not " } else { b"while " });
                self.condition(condition, *until)?;
                self.list(body)?;
                self.put(b"end");
            }
            CommandKind::Function { name, body } => {
                if !command.redirects.is_empty() {
                    return Err(Error::unsupported("redirections on a function definition"));
                }
                self.put(b"function ");
                let name = self.raw(name);
                literal(&mut self.0, name);
                self.put(b"\n");
                self.command(body)?;
                self.put(b"\nend");
            }
            CommandKind::Subshell(_) => return Err(Error::unsupported("subshell isolation")),
            CommandKind::Case { .. } => return Err(Error::unsupported("case syntax")),
            CommandKind::Arithmetic(_) => return Err(Error::unsupported("Bash arithmetic")),
            CommandKind::Conditional(_) => return Err(Error::unsupported("[[ conditions ]]")),
        }
        for redirect in &command.redirects {
            let operator: &[u8] = match redirect.kind {
                RedirectKind::Read => b"<",
                RedirectKind::Write | RedirectKind::Clobber => b">",
                RedirectKind::Append => b">>",
                RedirectKind::DupRead => b"<&",
                RedirectKind::DupWrite => b">&",
                RedirectKind::Both => b"&>",
                RedirectKind::ReadWrite => return Err(Error::unsupported("<> redirection")),
                RedirectKind::HereDoc | RedirectKind::HereDocStrip => {
                    return Err(Error::unsupported("here-documents"));
                }
                RedirectKind::HereString => return Err(Error::unsupported("here-strings")),
            };
            self.put(b" ");
            if let Some(fd) = redirect.fd {
                self.put(fd.to_string().as_bytes());
            }
            self.put(operator);
            self.put(&translate_word(self.raw(&redirect.target), self.1)?);
        }
        Ok(())
    }

    fn simple(&mut self, words: &[syntax::Word]) -> Result<(), Error> {
        let mut assignments = Vec::new();
        for word in words {
            let Some(assignment) = assignment(self.raw(word))? else {
                break;
            };
            assignments.push(assignment);
        }
        if words.is_empty() {
            self.put(b"builtin true");
        } else if assignments.len() == words.len() {
            if assignments.len() > 1 {
                self.put(b"begin\n");
            }
            for (index, (name, value)) in assignments.iter().enumerate() {
                if index != 0 {
                    self.put(b"\n");
                }
                self.put(b"builtin set -- ");
                self.put(name);
                self.put(b" ");
                let value = translate_word(value, self.1)?;
                self.put(if value.is_empty() { b"''" } else { &value });
            }
            if assignments.len() > 1 {
                self.put(b"\nend");
            }
        } else {
            for (index, word) in words.iter().enumerate() {
                if index != 0 {
                    self.put(b" ");
                }
                self.put(&translate_word(self.raw(word), self.1)?);
            }
        }
        Ok(())
    }
}

type Assignment<'a> = (&'a [u8], &'a [u8]);

fn assignment(raw: &[u8]) -> Result<Option<Assignment<'_>>, Error> {
    let Some(eq) = raw.iter().position(|&byte| byte == b'=') else {
        return Ok(None);
    };
    let name = &raw[..eq];
    let base_end = name
        .iter()
        .position(|byte| b"[+".contains(byte))
        .unwrap_or(name.len());
    if !syntax::identifier(&name[..base_end]) {
        return Ok(None);
    }
    if base_end != name.len() || raw.get(eq + 1) == Some(&b'(') {
        return Err(Error::unsupported("Bash array or append assignment"));
    }
    Ok(Some((name, &raw[eq + 1..])))
}

fn literal(output: &mut Vec<u8>, value: &[u8]) {
    output.push(b'\'');
    for &byte in value {
        if matches!(byte, b'\\' | b'\'') {
            output.push(b'\\');
        }
        output.push(byte);
    }
    output.push(b'\'');
}

fn translate_word(raw: &[u8], depth: usize) -> Result<Vec<u8>, Error> {
    if raw.is_empty() {
        return Ok(b"''".to_vec());
    }
    let mut output = Vec::new();
    let mut quote = None;
    let mut pos = 0;
    while pos < raw.len() {
        let byte = raw[pos];
        if quote == Some(b'\'') {
            if byte == b'\'' {
                quote = None;
            } else if byte == b'\\' {
                output.push(b'\\');
            }
            output.push(byte);
            pos += 1;
            continue;
        }
        if byte == b'"' || byte == b'\'' && quote.is_none() {
            quote = if quote.is_some() { None } else { Some(byte) };
            output.push(byte);
            pos += 1;
            continue;
        }
        match byte {
            b'\\' => {
                let Some(&next) = raw.get(pos + 1) else {
                    return Err(Error {
                        message: "unfinished escape".to_owned(),
                        incomplete: true,
                    });
                };
                if next == b'\n' {
                    // Both languages join an escaped newline without introducing a character.
                } else if quote.is_none() && next.is_ascii() {
                    // Fish's unquoted backslash also introduces escapes such as \a and \xNN.
                    literal(&mut output, &[next]);
                } else {
                    output.extend_from_slice(&raw[pos..pos + 2]);
                }
                pos += 2;
            }
            b'$' => {
                let next = raw.get(pos + 1).copied();
                match next {
                    Some(b'(') => {
                        if raw.get(pos + 2) == Some(&b'(') {
                            return Err(Error::unsupported("Bash arithmetic expansion"));
                        }
                        let end = syntax::expansion_end(&literal_backticks(raw), pos)?;
                        output.extend_from_slice(b"$(");
                        output.extend_from_slice(&translate_bytes(
                            &raw[pos + 2..end - 1],
                            depth + 1,
                        )?);
                        output.push(b')');
                        pos = end;
                    }
                    Some(b'{') => {
                        let end = syntax::expansion_end(&literal_backticks(raw), pos)?;
                        variable(&mut output, &raw[pos + 2..end - 1], quote, true)?;
                        pos = end;
                    }
                    Some(b'\'' | b'"' | b'[') => {
                        return Err(Error::unsupported("Bash quoting or arithmetic expansion"));
                    }
                    Some(byte) if byte.is_ascii_alphabetic() || byte == b'_' => {
                        let mut end = pos + 2;
                        while raw
                            .get(end)
                            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                        {
                            end += 1;
                        }
                        variable(&mut output, &raw[pos + 1..end], quote, false)?;
                        pos = end;
                    }
                    Some(byte) if b"0123456789@*?#$!-".contains(&byte) => {
                        variable(&mut output, &[byte], quote, false)?;
                        pos += 2;
                    }
                    _ => {
                        output.extend_from_slice(b"\\$");
                        pos += 1;
                    }
                }
            }
            b'(' | b')' if quote.is_none() => {
                return Err(Error::unsupported("extended globs or Bash array values"));
            }
            _ => {
                output.push(byte);
                pos += 1;
            }
        }
    }
    Ok(output)
}

fn variable(
    output: &mut Vec<u8>,
    name: &[u8],
    quote: Option<u8>,
    delimited: bool,
) -> Result<(), Error> {
    let mapped: &[u8] = match name {
        b"?" => b"status",
        b"$" => b"fish_pid",
        b"!" => b"last_pid",
        b"*" => b"argv",
        b"@" => {
            if quote == Some(b'"') {
                output.push(b'"');
            }
            output.extend_from_slice(b"$argv");
            if quote == Some(b'"') {
                output.push(b'"');
            } else {
                output.extend_from_slice(b"''");
            }
            return Ok(());
        }
        b"#" => {
            output.extend_from_slice(b"$(builtin count $argv)");
            return Ok(());
        }
        _ if !name.is_empty() && name[0] != b'0' && name.iter().all(u8::is_ascii_digit) => {
            output.extend_from_slice(b"$argv[");
            output.extend_from_slice(name);
            output.push(b']');
            return Ok(());
        }
        _ if syntax::identifier(name) => name,
        _ => return Err(Error::unsupported("this Bash parameter expansion")),
    };
    output.push(b'$');
    output.extend_from_slice(mapped);
    if delimited || matches!(name, b"?" | b"$" | b"!" | b"*") {
        output.extend_from_slice(if quote == Some(b'"') { b"\"\"" } else { b"''" });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(source: &str) -> String {
        #[cfg(not(windows))]
        crate::tests::prelude::test_init();
        translate(&WString::from(source)).unwrap().to_string()
    }

    #[test]
    #[cfg_attr(not(windows), crate::tests::prelude::serial)]
    fn assignments_and_fish_commands() {
        assert_eq!(
            check("repo='a b'; git -C \"${repo}\" switch main"),
            "builtin set -- repo 'a b'\ngit -C \"$repo\"\"\" switch main\n"
        );
        assert_eq!(
            check("X=a command env; set xs a 'b c'; printf '<%s>\\n' $xs[1..-1]"),
            "X=a command env\nset xs a 'b c'\nprintf '<%s>\\\\n' $xs[1..-1]\n"
        );
        assert_eq!(
            check("a= b=$a"),
            "begin\nbuiltin set -- a ''\nbuiltin set -- b $a\nend\n"
        );
        assert_eq!(
            check("..; alias ll='ls -l'; source config.fish"),
            "..\nalias ll='ls -l'\nsource config.fish\n"
        );
    }

    #[test]
    #[cfg_attr(not(windows), crate::tests::prelude::serial)]
    fn structural_control_flow() {
        assert_eq!(
            check("if false; then echo no; elif true; then echo yes; else echo no; fi"),
            "if false\necho no\nelse if true\necho yes\nelse\necho no\nend\n"
        );
        assert_eq!(
            check("for x in a 'b c'; do echo $x; done"),
            "for x in a 'b c'\necho $x\nend\n"
        );
        assert_eq!(
            check("until test -e stop; do sleep 1; done"),
            "while not test -e stop\nsleep 1\nend\n"
        );
        assert_eq!(
            check("f() { printf '%s\\n' \"$@\"; }; f a 'b c'"),
            "function 'f'\nbegin\nprintf '%s\\\\n' \"\"$argv\"\"\nend\nend\nf a 'b c'\n"
        );
    }

    #[test]
    #[cfg_attr(not(windows), crate::tests::prelude::serial)]
    fn substitutions_quotes_and_redirects() {
        assert_eq!(
            check("echo $(if true; then echo ü; fi) ${name}suffix \"${name}suffix\""),
            "echo $(if true\necho ü\nend\n) $name''suffix \"$name\"\"suffix\"\n"
        );
        assert_eq!(
            check(r#"printf '%s\n' \a 'c:\tmp' "\`" | cat 2>&1 && echo $? || echo failed &"#),
            "printf '%s\\\\n' 'a' 'c:\\\\tmp' \"\\`\" | cat 2>&1 && echo $status'' || echo failed &\n"
        );
        assert_eq!(
            check("{ echo hi; } >out; >empty"),
            "begin\necho hi\nend >out\nbuiltin true >empty\n"
        );
    }

    #[test]
    #[cfg_attr(not(windows), crate::tests::prelude::serial)]
    fn literal_backticks_keep_original_words_and_substitutions() {
        assert_eq!(
            check("echo `raw` \"unmatched `\""),
            "echo `raw` \"unmatched `\"\n"
        );
        assert_eq!(check("echo ` a b `"), "echo ` a b `\n");
        assert_eq!(
            check("echo $(printf '%s' '`') $?"),
            "echo $(printf '%s' '`'\n) $status''\n"
        );
        assert_eq!(
            check("x='ü`'; echo \"${x}`\"\necho next`"),
            "builtin set -- x 'ü`'\necho \"$x\"\"`\"\necho next`\n"
        );
    }

    #[test]
    #[cfg(not(windows))]
    #[crate::tests::prelude::serial]
    fn execution_uses_fish_lists_scope_and_functions() {
        use crate::io::{IoBufferfill, IoChain};
        use crate::tests::prelude::TestParser;

        crate::tests::prelude::test_init();
        let mut parser = TestParser::new();
        let source = translate(L!(r#"
            dialect_a='a b' dialect_b=$dialect_a
            printf '<%s>\n' $dialect_b
            set xs a 'b c'
            dialect_f() { printf '[%s]\n' "$@"; }
            dialect_f $xs
            printf '%s\n' \ü
            dialect_condition() { if set -l dialect_value yes; then printf '%s\n' $dialect_value; fi; }
            dialect_condition
            if true; false; then echo wrong; else echo last-condition; fi
            printf '{%s}\n' {a,b}
            false; printf '%s\n' $?suffix
        "#))
        .unwrap();
        let buffer = IoBufferfill::create().unwrap();
        let mut io = IoChain::new();
        io.push(buffer.clone());
        assert!(parser.eval(&source, &io).status.is_success());
        assert_eq!(
            IoBufferfill::finish(buffer).newline_serialized(),
            "<a b>\n[a]\n[b c]\nü\nyes\nlast-condition\n{a}\n{b}\n1suffix\n".as_bytes()
        );
        assert_eq!(
            check("echo ~/x {a,b} * $xs[1..-1]"),
            "echo ~/x {a,b} * $xs[1..-1]\n"
        );
    }

    #[test]
    #[cfg_attr(not(windows), crate::tests::prelude::serial)]
    fn rejects_whole_submission_and_nested_unsupported_syntax() {
        #[cfg(not(windows))]
        crate::tests::prelude::test_init();
        for source in [
            "echo should_not_run; echo ${x:-fallback}",
            "echo should_not_run; for bad` in a; do echo x; done",
            "echo should_not_run\necho $(echo ${x:-fallback})",
            "echo $(echo $( (true) ))",
            "echo $((1+2))",
            "((i++))",
            "[[ x == x ]]",
            "case x in x) true;; esac",
            "(cd /)",
            "cat <<EOF\nx\nEOF\n",
            "cat <<<hi",
            "a=(x y)",
            "a+=x",
            "a[0]=x",
            "echo $'x'",
            "echo <(true)",
            "echo @(a|b)",
            "echo ${a[0]}",
            "echo $0",
        ] {
            let error = translate(&WString::from(source)).expect_err(source);
            assert!(!error.incomplete, "{source}: {error:?}");
        }
        for source in ["if true; then", "echo \"", "echo $(true", "echo hi |"] {
            assert!(
                translate(&WString::from(source)).unwrap_err().incomplete,
                "{source}"
            );
        }
        let too_deep = format!("{}true{}", "echo $(".repeat(129), ")".repeat(129));
        assert!(translate(&WString::from(too_deep)).is_err());
    }
}
