// Copyright (C) 1989-2006 Free Software Foundation, Inc.
// Rust translation and source spans: Copyright (C) 2026 fish-shell contributors.
//
// Derived from GNU Bash 3.2.57 parse.y and command.h. This file may be
// redistributed and modified under GPL version 2 or (at your option) later.
// It comes WITHOUT ANY WARRANTY. See ../COPYING for the full license.

//! Non-executing Bash grammar. Words retain their original bytes and quoting.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::ops::Range;

#[path = "syntax_print.rs"]
mod print;
pub use print::format_function;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub raw: Vec<u8>,
    pub span: Range<usize>,
}

pub type List = Vec<ListItem>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    pub chain: AndOr,
    pub background: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndOr {
    pub first: Pipeline,
    pub rest: Vec<(LogicalOp, Pipeline)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pipeline {
    pub negate: bool,
    pub commands: Vec<Command>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub kind: CommandKind,
    pub redirects: Vec<Redirect>,
    pub span: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Simple(Vec<Word>),
    Group(List),
    Subshell(List),
    If {
        branches: Vec<(List, List)>,
        else_body: List,
    },
    For {
        name: Word,
        words: Option<Vec<Word>>,
        body: List,
    },
    While {
        until: bool,
        condition: List,
        body: List,
    },
    Case {
        word: Word,
        arms: Vec<CaseArm>,
    },
    Function {
        name: Word,
        body: Box<Command>,
    },
    Arithmetic(Word),
    Conditional(Vec<Word>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseArm {
    pub patterns: Vec<Word>,
    pub body: List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectKind {
    Read,
    Write,
    Append,
    Clobber,
    ReadWrite,
    DupRead,
    DupWrite,
    HereDoc,
    HereDocStrip,
    HereString,
    Both,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redirect {
    pub fd: Option<i32>,
    pub kind: RedirectKind,
    pub target: Word,
    pub heredoc: Option<HereDoc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HereDoc {
    pub body: Vec<u8>,
    pub expand: bool,
    pub strip_tabs: bool,
    id: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedUnit {
    pub body: List,
    /// Bytes consumed from the original source, including the terminating newline/heredocs.
    pub consumed: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    Incomplete,
    Invalid,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: Range<usize>,
    pub message: String,
}

impl ParseError {
    pub fn is_incomplete(&self) -> bool {
        self.kind == ParseErrorKind::Incomplete
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ParseError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyntaxStatus {
    Complete,
    Incomplete,
    Invalid(ParseError),
}

fn error(kind: ParseErrorKind, at: usize, message: impl Into<String>) -> ParseError {
    ParseError {
        kind,
        span: at..at,
        message: message.into(),
    }
}

/// Parse one Bash `inputunit`, stopping at an outer newline rather than parsing the file.
pub fn parse_one(input: &[u8]) -> Result<ParsedUnit, ParseError> {
    parse_one_with_aliases(input, &BTreeMap::new())
}

pub fn parse_one_with_aliases(
    input: &[u8],
    aliases: &BTreeMap<Vec<u8>, Vec<u8>>,
) -> Result<ParsedUnit, ParseError> {
    Parser::new(input, aliases).unit()
}

pub fn check(input: &[u8]) -> SyntaxStatus {
    check_with_aliases(input, &BTreeMap::new())
}

pub fn check_with_aliases(input: &[u8], aliases: &BTreeMap<Vec<u8>, Vec<u8>>) -> SyntaxStatus {
    let mut offset = 0;
    while offset < input.len() {
        match parse_one_with_aliases(&input[offset..], aliases) {
            Ok(unit) if unit.consumed > 0 => offset += unit.consumed,
            Ok(_) => break,
            Err(e) if e.is_incomplete() => return SyntaxStatus::Incomplete,
            Err(mut e) => {
                e.span.start += offset;
                e.span.end += offset;
                return SyntaxStatus::Invalid(e);
            }
        }
    }
    SyntaxStatus::Complete
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenKind {
    Word,
    Arithmetic,
    Newline,
    Semi,
    DoubleSemi,
    And,
    Or,
    Pipe,
    Amp,
    LeftParen,
    RightParen,
    Redirect(RedirectKind),
    AliasNext,
    Eof,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    word: Word,
    end: usize,
    alias_chain: Vec<Vec<u8>>,
    from_alias: bool,
    io_number: bool,
    force_alias: bool,
}

struct PendingDoc {
    delimiter: Vec<u8>,
    strip_tabs: bool,
    expand: bool,
    id: usize,
}

struct Lexer<'a> {
    input: &'a [u8],
    pos: usize,
    pending: Vec<PendingDoc>,
    documents: Vec<Vec<u8>>,
    nesting: usize,
    regexp: bool,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            pending: Vec::new(),
            documents: Vec::new(),
            nesting: 0,
            regexp: false,
        }
    }

    fn token(&self, start: usize, end: usize, kind: TokenKind) -> Token {
        let io_number = start != end
            && self.input[start..end].iter().all(u8::is_ascii_digit)
            && matches!(self.input.get(end), Some(b'<' | b'>'));
        Token {
            kind,
            word: Word {
                raw: self.input[start..end].to_vec(),
                span: start..end,
            },
            end: self.pos,
            alias_chain: Vec::new(),
            from_alias: false,
            io_number,
            force_alias: false,
        }
    }

    fn next(&mut self) -> Result<Token, ParseError> {
        let regexp = std::mem::take(&mut self.regexp);
        loop {
            while matches!(self.input.get(self.pos), Some(b' ' | b'\t')) {
                self.pos += 1;
            }
            if self.input.get(self.pos..self.pos + 2) == Some(b"\\\n") {
                self.pos += 2;
                continue;
            }
            if self.input.get(self.pos) == Some(&b'#') {
                while !matches!(self.input.get(self.pos), None | Some(b'\n')) {
                    self.pos += 1;
                }
            }
            break;
        }
        let start = self.pos;
        let Some(&byte) = self.input.get(start) else {
            if !self.pending.is_empty() {
                return Err(error(
                    ParseErrorKind::Incomplete,
                    start,
                    "unexpected EOF before here-document",
                ));
            }
            return Ok(self.token(start, start, TokenKind::Eof));
        };
        if byte == 0 {
            return Err(error(
                ParseErrorKind::Invalid,
                start,
                "NUL byte in shell input",
            ));
        }
        if byte == b'\n' {
            self.pos += 1;
            let newline_end = self.pos;
            self.read_heredocs()?;
            return Ok(self.token(start, newline_end, TokenKind::Newline));
        }
        if !regexp && self.input[start..].starts_with(b"((") {
            self.pos = arithmetic_end(self.input, start + 2, self.nesting)?;
            let mut token = self.token(start + 2, self.pos - 2, TokenKind::Arithmetic);
            token.end = self.pos;
            return Ok(token);
        }
        for unsupported in [b"|&".as_slice(), b"&>>", b";;&", b";&"] {
            if !regexp && self.input[start..].starts_with(unsupported) {
                return Err(error(
                    ParseErrorKind::Unsupported,
                    start,
                    "operator is not supported by the Bash 3.2 grammar",
                ));
            }
        }
        let operators: &[(&[u8], TokenKind)] = &[
            (b"&&", TokenKind::And),
            (b"||", TokenKind::Or),
            (b";;", TokenKind::DoubleSemi),
            (b"<<<", TokenKind::Redirect(RedirectKind::HereString)),
            (b"<<-", TokenKind::Redirect(RedirectKind::HereDocStrip)),
            (b"<<", TokenKind::Redirect(RedirectKind::HereDoc)),
            (b">>", TokenKind::Redirect(RedirectKind::Append)),
            (b">|", TokenKind::Redirect(RedirectKind::Clobber)),
            (b"<>", TokenKind::Redirect(RedirectKind::ReadWrite)),
            (b"<&", TokenKind::Redirect(RedirectKind::DupRead)),
            (b">&", TokenKind::Redirect(RedirectKind::DupWrite)),
            (b"&>", TokenKind::Redirect(RedirectKind::Both)),
            (b";", TokenKind::Semi),
            (b"&", TokenKind::Amp),
            (b"|", TokenKind::Pipe),
            (b"(", TokenKind::LeftParen),
            (b")", TokenKind::RightParen),
            (b"<", TokenKind::Redirect(RedirectKind::Read)),
            (b">", TokenKind::Redirect(RedirectKind::Write)),
        ];
        if self.input[start..].starts_with(b"<(") || self.input[start..].starts_with(b">(") {
            return Err(error(
                ParseErrorKind::Unsupported,
                start,
                "process substitution is not supported yet",
            ));
        }
        for (spelling, kind) in operators {
            if regexp && matches!(byte, b'(' | b'|') {
                break;
            }
            if self.input[start..].starts_with(spelling) {
                self.pos += spelling.len();
                return Ok(self.token(start, self.pos, kind.clone()));
            }
        }
        while let Some(&byte) = self.input.get(self.pos) {
            match byte {
                0 => {
                    return Err(error(
                        ParseErrorKind::Invalid,
                        self.pos,
                        "NUL byte in shell input",
                    ));
                }
                b'|' if regexp => self.pos += 1,
                b'(' if regexp => {
                    self.pos = pattern_group_end(self.input, self.pos + 1, self.nesting)?;
                }
                b'?' | b'*' | b'+' | b'@' | b'!' if self.input.get(self.pos + 1) == Some(&b'(') => {
                    self.pos = pattern_group_end(self.input, self.pos + 2, self.nesting)?;
                }
                b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'<' | b'>' | b')' => break,
                b'(' if assignment_prefix(&self.input[start..self.pos]) => {
                    self.pos = array_end(self.input, self.pos + 1, self.nesting)?;
                }
                b'(' => break,
                b'\\' => {
                    if self.pos + 1 == self.input.len() {
                        return Err(error(
                            ParseErrorKind::Incomplete,
                            self.pos,
                            "unexpected EOF after backslash",
                        ));
                    }
                    self.pos += 2;
                }
                b'\'' | b'"' | b'`' => {
                    self.pos = quoted_end(self.input, self.pos, byte, self.nesting)?;
                }
                b'$' if matches!(self.input.get(self.pos + 1), Some(b'(' | b'{')) => {
                    self.pos = expansion_end_at(self.input, self.pos, self.nesting + 1)?;
                }
                b'$' if self.input.get(self.pos + 1) == Some(&b'\'') => {
                    self.pos = ansi_quote_end(self.input, self.pos + 1)?;
                }
                _ => self.pos += 1,
            }
        }
        Ok(self.token(start, self.pos, TokenKind::Word))
    }

    fn read_heredocs(&mut self) -> Result<(), ParseError> {
        for pending in std::mem::take(&mut self.pending) {
            let mut body = Vec::new();
            loop {
                if self.pos == self.input.len() {
                    return Err(error(
                        ParseErrorKind::Incomplete,
                        self.pos,
                        format!(
                            "unexpected EOF in here-document (wanted {})",
                            String::from_utf8_lossy(&pending.delimiter)
                        ),
                    ));
                }
                let mut line = Vec::new();
                loop {
                    let start = self.pos;
                    while self.pos < self.input.len() && self.input[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    let end = self.pos;
                    let newline = self.input.get(self.pos) == Some(&b'\n');
                    if newline {
                        self.pos += 1;
                    }
                    let mut part = &self.input[start..end];
                    if pending.strip_tabs {
                        while part.starts_with(b"\t") {
                            part = &part[1..];
                        }
                    }
                    let backslashes = part.iter().rev().take_while(|&&c| c == b'\\').count();
                    if pending.expand && newline && backslashes % 2 == 1 {
                        line.extend_from_slice(&part[..part.len() - 1]);
                        continue;
                    }
                    line.extend_from_slice(part);
                    if line == pending.delimiter {
                        break;
                    }
                    body.extend_from_slice(&line);
                    if newline {
                        body.push(b'\n');
                    }
                    break;
                }
                if line == pending.delimiter {
                    break;
                }
            }
            self.documents[pending.id] = body;
        }
        Ok(())
    }
}

fn assignment_prefix(raw: &[u8]) -> bool {
    let Some(name) = raw.strip_suffix(b"=") else {
        return false;
    };
    identifier(name.strip_suffix(b"+").unwrap_or(name))
}

pub fn identifier(name: &[u8]) -> bool {
    matches!(name.first(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && name[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

fn quoted_end(input: &[u8], start: usize, quote: u8, nesting: usize) -> Result<usize, ParseError> {
    if nesting > 128 {
        return Err(error(
            ParseErrorKind::Unsupported,
            start,
            "expansion nesting limit exceeded",
        ));
    }
    let mut pos = start + 1;
    while let Some(&byte) = input.get(pos) {
        if byte == 0 {
            return Err(error(
                ParseErrorKind::Invalid,
                pos,
                "NUL byte in shell input",
            ));
        }
        if byte == quote {
            return Ok(pos + 1);
        }
        if quote != b'\'' && byte == b'\\' {
            pos += 2;
            continue;
        }
        if quote == b'"' && byte == b'`' {
            pos = quoted_end(input, pos, b'`', nesting + 1)?;
            continue;
        }
        if quote == b'"' && byte == b'$' && matches!(input.get(pos + 1), Some(b'(' | b'{')) {
            pos = expansion_end_at(input, pos, nesting + 1)?;
            continue;
        }
        pos += 1;
    }
    Err(error(
        ParseErrorKind::Incomplete,
        start,
        "unexpected EOF while looking for matching quote",
    ))
}

fn ansi_quote_end(input: &[u8], start: usize) -> Result<usize, ParseError> {
    let mut pos = start + 1;
    while let Some(&byte) = input.get(pos) {
        if byte == b'\'' {
            return Ok(pos + 1);
        }
        if byte == b'\\' {
            pos += 1;
        }
        pos += 1;
    }
    Err(error(
        ParseErrorKind::Incomplete,
        start,
        "unexpected EOF in ANSI-C quote",
    ))
}

fn arithmetic_end(input: &[u8], mut pos: usize, nesting: usize) -> Result<usize, ParseError> {
    let start = pos;
    let mut depth = 1;
    while let Some(&byte) = input.get(pos) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    if input.get(pos + 1) == Some(&b')') {
                        return Ok(pos + 2);
                    }
                    return Err(error(
                        ParseErrorKind::Invalid,
                        pos,
                        "expected )) after arithmetic expression",
                    ));
                }
            }
            b'\'' | b'"' | b'`' => {
                pos = quoted_end(input, pos, byte, nesting + 1)?;
                continue;
            }
            b'\\' => {
                pos += 2;
                continue;
            }
            b'$' if matches!(input.get(pos + 1), Some(b'(' | b'{')) => {
                pos = expansion_end_at(input, pos, nesting + 1)?;
                continue;
            }
            _ => {}
        }
        pos += 1;
    }
    Err(error(
        ParseErrorKind::Incomplete,
        start,
        "unexpected EOF in arithmetic expression",
    ))
}

fn array_end(input: &[u8], mut pos: usize, nesting: usize) -> Result<usize, ParseError> {
    let start = pos;
    while let Some(&byte) = input.get(pos) {
        match byte {
            b')' => return Ok(pos + 1),
            b'(' => {
                return Err(error(
                    ParseErrorKind::Unsupported,
                    pos,
                    "nested compound array assignment is unsupported",
                ));
            }
            b'\'' | b'"' | b'`' => {
                pos = quoted_end(input, pos, byte, nesting + 1)?;
                continue;
            }
            b'\\' => {
                pos += 2;
                continue;
            }
            b'$' if matches!(input.get(pos + 1), Some(b'(' | b'{')) => {
                pos = expansion_end_at(input, pos, nesting + 1)?;
                continue;
            }
            _ => {}
        }
        pos += 1;
    }
    Err(error(
        ParseErrorKind::Incomplete,
        start,
        "unexpected EOF in compound array assignment",
    ))
}

fn pattern_group_end(input: &[u8], mut pos: usize, nesting: usize) -> Result<usize, ParseError> {
    let start = pos;
    let mut depth = 1;
    while let Some(&byte) = input.get(pos) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(pos + 1);
                }
            }
            b'\'' | b'"' | b'`' => {
                pos = quoted_end(input, pos, byte, nesting + 1)?;
                continue;
            }
            b'\\' => {
                pos += 2;
                continue;
            }
            b'$' if matches!(input.get(pos + 1), Some(b'(' | b'{')) => {
                pos = expansion_end_at(input, pos, nesting + 1)?;
                continue;
            }
            _ => {}
        }
        pos += 1;
    }
    Err(error(
        ParseErrorKind::Incomplete,
        start,
        "unexpected EOF in pattern group",
    ))
}

/// Exclusive end of `${...}`, `$(...)`, or `$((...))`, without performing expansions.
pub fn expansion_end(input: &[u8], start: usize) -> Result<usize, ParseError> {
    expansion_end_at(input, start, 0)
}

fn expansion_end_at(input: &[u8], start: usize, nesting: usize) -> Result<usize, ParseError> {
    if nesting > 128 {
        return Err(error(
            ParseErrorKind::Unsupported,
            start,
            "expansion nesting limit exceeded",
        ));
    }
    if input.get(start..start + 3) == Some(b"$((") {
        return arithmetic_end(input, start + 3, nesting);
    }
    if input.get(start..start + 2) == Some(b"$(") {
        let aliases = BTreeMap::new();
        let mut parser = Parser::new(&input[start + 2..], &aliases);
        parser.lexer.nesting = nesting + 1;
        let result = (|| {
            parser.list(&[], true, false, true)?;
            parser.expect_kind(TokenKind::RightParen, ")")?;
            Ok(start + 2 + parser.consumed)
        })();
        return result.map_err(|mut e: ParseError| {
            e.span.start += start + 2;
            e.span.end += start + 2;
            e
        });
    }
    if input.get(start..start + 2) != Some(b"${") {
        return Err(error(
            ParseErrorKind::Invalid,
            start,
            "expected shell expansion",
        ));
    }
    let mut pos = start + 2;
    while let Some(&byte) = input.get(pos) {
        match byte {
            b'}' => return Ok(pos + 1),
            b'\'' | b'"' | b'`' => {
                pos = quoted_end(input, pos, byte, nesting + 1)?;
                continue;
            }
            b'\\' => {
                pos += 2;
                continue;
            }
            b'$' if matches!(input.get(pos + 1), Some(b'(' | b'{')) => {
                pos = expansion_end_at(input, pos, nesting + 1)?;
                continue;
            }
            _ => {}
        }
        pos += 1;
    }
    Err(error(
        ParseErrorKind::Incomplete,
        start,
        "unexpected EOF in parameter expansion",
    ))
}

struct Parser<'a, 'b> {
    lexer: Lexer<'a>,
    tokens: VecDeque<Token>,
    aliases: &'b BTreeMap<Vec<u8>, Vec<u8>>,
    consumed: usize,
    depth: usize,
}

impl<'a, 'b> Parser<'a, 'b> {
    fn new(input: &'a [u8], aliases: &'b BTreeMap<Vec<u8>, Vec<u8>>) -> Self {
        Self {
            lexer: Lexer::new(input),
            tokens: VecDeque::new(),
            aliases,
            consumed: 0,
            depth: 0,
        }
    }

    fn peek_n(&mut self, n: usize) -> Result<&Token, ParseError> {
        loop {
            while self.tokens.len() <= n {
                self.tokens.push_back(self.lexer.next()?);
            }
            let Some(index) = self
                .tokens
                .iter()
                .take(n + 1)
                .position(|t| t.kind == TokenKind::AliasNext)
            else {
                break;
            };
            self.tokens.remove(index);
            if self.tokens.len() == index {
                self.tokens.push_back(self.lexer.next()?);
            }
            self.tokens[index].force_alias = true;
        }
        Ok(&self.tokens[n])
    }
    fn peek(&mut self) -> Result<&Token, ParseError> {
        self.peek_n(0)
    }
    fn bump(&mut self) -> Result<Token, ParseError> {
        self.peek()?;
        let token = self.tokens.pop_front().unwrap();
        self.consumed = self.consumed.max(token.end);
        Ok(token)
    }
    fn at_word(&mut self, word: &[u8]) -> Result<bool, ParseError> {
        let token = self.peek()?;
        Ok(token.kind == TokenKind::Word && token.word.raw == word)
    }
    fn at(&mut self, kind: TokenKind) -> Result<bool, ParseError> {
        Ok(self.peek()?.kind == kind)
    }
    fn unexpected(&mut self, message: impl Into<String>) -> ParseError {
        match self.peek() {
            Ok(token) => error(
                if token.kind == TokenKind::Eof {
                    ParseErrorKind::Incomplete
                } else {
                    ParseErrorKind::Invalid
                },
                token.word.span.start,
                message,
            ),
            Err(e) => e,
        }
    }
    fn expect_kind(&mut self, kind: TokenKind, spelling: &str) -> Result<Token, ParseError> {
        if !self.at(kind)? {
            return Err(self.unexpected(format!("expected {spelling}")));
        }
        self.bump()
    }
    fn expect_word(&mut self, spelling: &[u8]) -> Result<(), ParseError> {
        if !self.at_word(spelling)? {
            return Err(self.unexpected(format!("expected {}", String::from_utf8_lossy(spelling))));
        }
        self.bump()?;
        Ok(())
    }
    fn word(&mut self) -> Result<Word, ParseError> {
        Ok(self.expect_kind(TokenKind::Word, "word")?.word)
    }
    fn newlines(&mut self) -> Result<(), ParseError> {
        while self.at(TokenKind::Newline)? {
            self.bump()?;
        }
        Ok(())
    }

    fn unit(mut self) -> Result<ParsedUnit, ParseError> {
        let mut body = self.list(&[], false, true, true)?;
        if self.at(TokenKind::Newline)? || self.at(TokenKind::Eof)? {
            self.bump()?;
        } else {
            return Err(self.unexpected("unexpected token after command"));
        }
        fill_heredocs(&mut body, &self.lexer.documents);
        Ok(ParsedUnit {
            body,
            consumed: self.consumed,
        })
    }

    fn stop(&mut self, words: &[&[u8]], paren: bool, case: bool) -> Result<bool, ParseError> {
        let token = self.peek()?;
        Ok(token.kind == TokenKind::Eof
            || (paren && token.kind == TokenKind::RightParen)
            || (case && token.kind == TokenKind::DoubleSemi)
            || (token.kind == TokenKind::Word && words.iter().any(|word| *word == token.word.raw)))
    }

    fn list(
        &mut self,
        stops: &[&[u8]],
        paren: bool,
        outer: bool,
        allow_empty: bool,
    ) -> Result<List, ParseError> {
        let mut list = Vec::new();
        if !outer {
            self.newlines()?;
        }
        loop {
            self.expand_alias()?;
            if self.stop(stops, paren, stops.contains(&b"esac".as_slice()))?
                || (outer && self.at(TokenKind::Newline)? && !self.peek()?.from_alias)
            {
                if list.is_empty() && !allow_empty {
                    return Err(self.unexpected("expected command"));
                }
                return Ok(list);
            }
            let chain = self.and_or()?;
            let background = self.at(TokenKind::Amp)?;
            list.push(ListItem { chain, background });
            if background || self.at(TokenKind::Semi)? {
                self.bump()?;
                if !outer {
                    self.newlines()?;
                }
            } else if self.at(TokenKind::Newline)? {
                if outer && !self.peek()?.from_alias {
                    return Ok(list);
                }
                if outer {
                    while self.at(TokenKind::Newline)? && self.peek()?.from_alias {
                        self.bump()?;
                    }
                } else {
                    self.newlines()?;
                }
            } else if self.stop(stops, paren, stops.contains(&b"esac".as_slice()))? {
                // Parenthesized/case command lists may end directly at their delimiter.
                if !paren && !stops.contains(&b"esac".as_slice()) && !self.at(TokenKind::Eof)? {
                    return Err(self.unexpected("expected ; or newline before reserved word"));
                }
                return Ok(list);
            } else {
                return Err(self.unexpected("expected command separator"));
            }
        }
    }

    fn and_or(&mut self) -> Result<AndOr, ParseError> {
        let first = self.pipeline()?;
        let mut rest = Vec::new();
        loop {
            let op = match self.peek()?.kind {
                TokenKind::And => LogicalOp::And,
                TokenKind::Or => LogicalOp::Or,
                _ => break,
            };
            self.bump()?;
            self.newlines()?;
            rest.push((op, self.pipeline()?));
        }
        Ok(AndOr { first, rest })
    }

    fn pipeline(&mut self) -> Result<Pipeline, ParseError> {
        self.expand_alias()?;
        let negate = self.at_word(b"!")?;
        if negate {
            self.bump()?;
        }
        if self.at_word(b"time")? || self.at_word(b"select")? || self.at_word(b"coproc")? {
            return Err(error(
                ParseErrorKind::Unsupported,
                self.peek()?.word.span.start,
                "this compound command is not supported yet",
            ));
        }
        let mut commands = vec![self.command()?];
        while self.at(TokenKind::Pipe)? {
            self.bump()?;
            self.newlines()?;
            commands.push(self.command()?);
        }
        Ok(Pipeline { negate, commands })
    }

    fn expand_alias(&mut self) -> Result<(), ParseError> {
        for _ in 0..128 {
            let token = self.peek()?.clone();
            if token.kind != TokenKind::Word || token.alias_chain.contains(&token.word.raw) {
                return Ok(());
            }
            let Some(value) = self.aliases.get(&token.word.raw) else {
                return Ok(());
            };
            let mut lexer = Lexer::new(value);
            let mut expanded = Vec::new();
            loop {
                let mut part = lexer.next().map_err(|_| {
                    error(
                        ParseErrorKind::Unsupported,
                        token.word.span.start,
                        "alias with an unfinished token is unsupported",
                    )
                })?;
                if part.kind == TokenKind::Eof {
                    break;
                }
                if matches!(
                    part.kind,
                    TokenKind::Redirect(RedirectKind::HereDoc | RedirectKind::HereDocStrip)
                ) {
                    return Err(error(
                        ParseErrorKind::Unsupported,
                        token.word.span.start,
                        "here-document in alias replacement is unsupported",
                    ));
                }
                part.word.span = token.word.span.clone();
                part.end = token.end;
                part.from_alias = true;
                part.alias_chain.clone_from(&token.alias_chain);
                part.alias_chain.push(token.word.raw.clone());
                expanded.push(part);
            }
            self.bump()?;
            if matches!(value.last(), Some(b' ' | b'\t')) {
                let mut next = token;
                next.kind = TokenKind::AliasNext;
                expanded.push(next);
            }
            for part in expanded.into_iter().rev() {
                self.tokens.push_front(part);
            }
        }
        Err(error(
            ParseErrorKind::Unsupported,
            self.consumed,
            "alias expansion nesting limit exceeded",
        ))
    }

    fn command(&mut self) -> Result<Command, ParseError> {
        self.depth += 1;
        if self.depth > 128 {
            return Err(error(
                ParseErrorKind::Unsupported,
                self.consumed,
                "command nesting limit exceeded",
            ));
        }
        self.expand_alias()?;
        let start = self.peek()?.word.span.start;
        let kind = if self.at_word(b"{")? {
            self.bump()?;
            let body = self.list(&[b"}"], false, false, false)?;
            self.expect_word(b"}")?;
            CommandKind::Group(body)
        } else if self.at(TokenKind::LeftParen)? {
            self.bump()?;
            let body = self.list(&[], true, false, false)?;
            self.expect_kind(TokenKind::RightParen, ")")?;
            CommandKind::Subshell(body)
        } else if self.at_word(b"if")? {
            self.if_command()?
        } else if self.at_word(b"while")? || self.at_word(b"until")? {
            self.while_command()?
        } else if self.at_word(b"for")? {
            self.for_command()?
        } else if self.at_word(b"case")? {
            self.case_command()?
        } else if self.at_word(b"function")? {
            self.function_command(true)?
        } else if self.at(TokenKind::Arithmetic)? {
            CommandKind::Arithmetic(self.bump()?.word)
        } else if self.at_word(b"[[")? {
            self.conditional()?
        } else if self.at(TokenKind::Word)? && self.peek_n(1)?.kind == TokenKind::LeftParen {
            self.function_command(false)?
        } else {
            let mut words = Vec::new();
            let mut redirects = Vec::new();
            let mut command_position = true;
            loop {
                if command_position || self.peek()?.force_alias {
                    self.expand_alias()?;
                }
                if self.is_redirect()? {
                    redirects.push(self.redirect()?);
                } else if self.at(TokenKind::Word)? {
                    if words.is_empty() && reserved_end(&self.peek()?.word.raw) {
                        return Err(self.unexpected("unexpected reserved word"));
                    }
                    let word = self.bump()?.word;
                    if command_position {
                        command_position = word
                            .raw
                            .iter()
                            .position(|&b| b == b'=')
                            .is_some_and(|at| assignment_prefix(&word.raw[..=at]));
                    }
                    words.push(word);
                } else {
                    break;
                }
            }
            if words.is_empty() && redirects.is_empty() {
                return Err(self.unexpected("expected command"));
            }
            self.depth -= 1;
            return Ok(Command {
                kind: CommandKind::Simple(words),
                redirects,
                span: start..self.consumed,
            });
        };
        let mut redirects = Vec::new();
        while self.is_redirect()? {
            redirects.push(self.redirect()?);
        }
        self.depth -= 1;
        Ok(Command {
            kind,
            redirects,
            span: start..self.consumed,
        })
    }

    fn is_redirect(&mut self) -> Result<bool, ParseError> {
        let token = self.peek()?.clone();
        if matches!(token.kind, TokenKind::Redirect(_)) {
            return Ok(true);
        }
        if token.kind == TokenKind::Word && token.io_number {
            let next = self.peek_n(1)?;
            return Ok(matches!(next.kind, TokenKind::Redirect(_)));
        }
        Ok(false)
    }

    fn redirect(&mut self) -> Result<Redirect, ParseError> {
        let fd = if self.at(TokenKind::Word)? {
            let word = self.bump()?.word;
            Some(
                std::str::from_utf8(&word.raw)
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                    .ok_or_else(|| {
                        error(
                            ParseErrorKind::Invalid,
                            word.span.start,
                            "file descriptor is out of range",
                        )
                    })?,
            )
        } else {
            None
        };
        let token = self.bump()?;
        let TokenKind::Redirect(kind) = token.kind else {
            unreachable!()
        };
        let target = self.word()?;
        let heredoc = if matches!(kind, RedirectKind::HereDoc | RedirectKind::HereDocStrip) {
            let (delimiter, expand) = heredoc_delimiter(&target)?;
            let id = self.lexer.documents.len();
            self.lexer.documents.push(Vec::new());
            let strip_tabs = kind == RedirectKind::HereDocStrip;
            self.lexer.pending.push(PendingDoc {
                delimiter,
                strip_tabs,
                expand,
                id,
            });
            Some(HereDoc {
                body: Vec::new(),
                expand,
                strip_tabs,
                id,
            })
        } else {
            None
        };
        Ok(Redirect {
            fd,
            kind,
            target,
            heredoc,
        })
    }

    fn if_command(&mut self) -> Result<CommandKind, ParseError> {
        self.bump()?;
        let mut branches = Vec::new();
        loop {
            let condition = self.list(&[b"then"], false, false, false)?;
            self.expect_word(b"then")?;
            let body = self.list(&[b"elif", b"else", b"fi"], false, false, false)?;
            branches.push((condition, body));
            if self.at_word(b"elif")? {
                self.bump()?;
            } else {
                break;
            }
        }
        let else_body = if self.at_word(b"else")? {
            self.bump()?;
            self.list(&[b"fi"], false, false, false)?
        } else {
            Vec::new()
        };
        self.expect_word(b"fi")?;
        Ok(CommandKind::If {
            branches,
            else_body,
        })
    }

    fn while_command(&mut self) -> Result<CommandKind, ParseError> {
        let until = self.bump()?.word.raw == b"until";
        let condition = self.list(&[b"do"], false, false, false)?;
        self.expect_word(b"do")?;
        let body = self.list(&[b"done"], false, false, false)?;
        self.expect_word(b"done")?;
        Ok(CommandKind::While {
            until,
            condition,
            body,
        })
    }

    fn for_command(&mut self) -> Result<CommandKind, ParseError> {
        self.bump()?;
        if self.at(TokenKind::Arithmetic)? {
            return Err(error(
                ParseErrorKind::Unsupported,
                self.peek()?.word.span.start,
                "arithmetic for loops are not supported yet",
            ));
        }
        let name = self.word()?;
        if !identifier(&name.raw) {
            return Err(error(
                ParseErrorKind::Invalid,
                name.span.start,
                "invalid for-loop variable",
            ));
        }
        self.newlines()?;
        let words = if self.at_word(b"in")? {
            self.bump()?;
            let mut words = Vec::new();
            while self.at(TokenKind::Word)? {
                words.push(self.word()?);
            }
            Some(words)
        } else {
            None
        };
        if self.at(TokenKind::Semi)? || self.at(TokenKind::Newline)? {
            self.bump()?;
            self.newlines()?;
        } else if words.is_some() {
            return Err(self.unexpected("expected ; or newline after for words"));
        }
        let braces = self.at_word(b"{")?;
        self.expect_word(if braces { b"{" } else { b"do" })?;
        let terminator: &[u8] = if braces { b"}" } else { b"done" };
        let body = self.list(&[terminator], false, false, false)?;
        self.expect_word(terminator)?;
        Ok(CommandKind::For { name, words, body })
    }

    fn case_command(&mut self) -> Result<CommandKind, ParseError> {
        self.bump()?;
        let word = self.word()?;
        self.newlines()?;
        self.expect_word(b"in")?;
        self.newlines()?;
        let mut arms = Vec::new();
        while !self.at_word(b"esac")? {
            if self.at(TokenKind::LeftParen)? {
                self.bump()?;
            }
            let mut patterns = vec![self.word()?];
            while self.at(TokenKind::Pipe)? {
                self.bump()?;
                patterns.push(self.word()?);
            }
            self.expect_kind(TokenKind::RightParen, ")")?;
            let body = self.list(&[b"esac"], false, false, true)?;
            arms.push(CaseArm { patterns, body });
            if self.at(TokenKind::DoubleSemi)? {
                self.bump()?;
                self.newlines()?;
            } else {
                break;
            }
        }
        self.expect_word(b"esac")?;
        Ok(CommandKind::Case { word, arms })
    }

    fn function_command(&mut self, keyword: bool) -> Result<CommandKind, ParseError> {
        if keyword {
            self.bump()?;
        }
        let name = self.word()?;
        if name.raw.contains(&b'=') || name.raw.iter().any(|b| b"'\"\\$`".contains(b)) {
            return Err(error(
                ParseErrorKind::Invalid,
                name.span.start,
                "invalid function name",
            ));
        }
        if !keyword || self.at(TokenKind::LeftParen)? {
            self.expect_kind(TokenKind::LeftParen, "(")?;
            self.expect_kind(TokenKind::RightParen, ")")?;
        }
        self.newlines()?;
        let body = self.command()?;
        if matches!(
            body.kind,
            CommandKind::Simple(_) | CommandKind::Function { .. }
        ) {
            return Err(error(
                ParseErrorKind::Invalid,
                body.span.start,
                "function body must be a compound command",
            ));
        }
        Ok(CommandKind::Function {
            name,
            body: Box::new(body),
        })
    }

    fn conditional(&mut self) -> Result<CommandKind, ParseError> {
        self.bump()?;
        let mut words = Vec::new();
        while !self.at_word(b"]]")? {
            if self.at(TokenKind::Eof)? {
                return Err(self.unexpected("unexpected EOF in [[ condition ]]"));
            }
            let token = self.bump()?;
            if token.word.raw == b"=~" {
                self.lexer.regexp = true;
            }
            if token.kind == TokenKind::Newline {
                continue;
            }
            if matches!(
                token.kind,
                TokenKind::Semi | TokenKind::DoubleSemi | TokenKind::Amp
            ) {
                return Err(error(
                    ParseErrorKind::Invalid,
                    token.word.span.start,
                    "unexpected operator in [[ condition ]]",
                ));
            }
            words.push(token.word);
        }
        self.bump()?;
        if words.is_empty() {
            return Err(self.unexpected("empty [[ condition ]]"));
        }
        validate_conditional(&words)?;
        Ok(CommandKind::Conditional(words))
    }
}

fn validate_conditional(words: &[Word]) -> Result<(), ParseError> {
    fn atom(words: &[Word], pos: &mut usize, depth: usize) -> Result<(), ParseError> {
        if depth > 128 {
            return Err(error(
                ParseErrorKind::Unsupported,
                0,
                "conditional nesting limit exceeded",
            ));
        }
        let at = words
            .get(*pos)
            .map_or_else(|| words.last().map_or(0, |w| w.span.end), |w| w.span.start);
        let Some(word) = words.get(*pos) else {
            return Err(error(
                ParseErrorKind::Invalid,
                at,
                "missing conditional operand",
            ));
        };
        *pos += 1;
        if word.raw == b"!" {
            return atom(words, pos, depth + 1);
        }
        if word.raw == b"(" {
            expression(words, pos, depth + 1)?;
            if words.get(*pos).is_none_or(|w| w.raw != b")") {
                return Err(error(
                    ParseErrorKind::Invalid,
                    at,
                    "expected ) in conditional",
                ));
            }
            *pos += 1;
            return Ok(());
        }
        if [b")".as_slice(), b"&&", b"||", b"<", b">", b"|"].contains(&word.raw.as_slice()) {
            return Err(error(
                ParseErrorKind::Invalid,
                at,
                "unexpected conditional operator",
            ));
        }
        let unary = word.raw.len() == 2
            && word.raw[0] == b'-'
            && b"abcdefghkLnOoprsStuwxzG".contains(&word.raw[1]);
        let binary = words.get(*pos).is_some_and(|w| {
            [
                b"=".as_slice(),
                b"==",
                b"!=",
                b"<",
                b">",
                b"=~",
                b"-eq",
                b"-ne",
                b"-lt",
                b"-le",
                b"-gt",
                b"-ge",
                b"-ef",
                b"-nt",
                b"-ot",
            ]
            .contains(&w.raw.as_slice())
        });
        if unary || binary {
            if binary && !unary {
                *pos += 1;
            }
            let Some(operand) = words.get(*pos) else {
                return Err(error(
                    ParseErrorKind::Invalid,
                    at,
                    "missing conditional operand",
                ));
            };
            if [b"(".as_slice(), b")", b"&&", b"||", b"<", b">", b"|"]
                .contains(&operand.raw.as_slice())
            {
                return Err(error(
                    ParseErrorKind::Invalid,
                    operand.span.start,
                    "expected word as conditional operand",
                ));
            }
            *pos += 1;
        }
        Ok(())
    }
    fn expression(words: &[Word], pos: &mut usize, depth: usize) -> Result<(), ParseError> {
        atom(words, pos, depth)?;
        while words
            .get(*pos)
            .is_some_and(|w| w.raw == b"&&" || w.raw == b"||")
        {
            *pos += 1;
            atom(words, pos, depth)?;
        }
        Ok(())
    }
    let mut pos = 0;
    expression(words, &mut pos, 0)?;
    if pos != words.len() {
        return Err(error(
            ParseErrorKind::Invalid,
            words[pos].span.start,
            "unexpected word in conditional",
        ));
    }
    Ok(())
}

fn reserved_end(word: &[u8]) -> bool {
    [
        b"then".as_slice(),
        b"else",
        b"elif",
        b"fi",
        b"do",
        b"done",
        b"esac",
        b"}",
    ]
    .contains(&word)
}

fn heredoc_delimiter(word: &Word) -> Result<(Vec<u8>, bool), ParseError> {
    let mut result = Vec::new();
    let mut quote = None;
    let mut quoted = false;
    let mut pos = 0;
    while pos < word.raw.len() {
        let byte = word.raw[pos];
        if quote == Some(byte) {
            quote = None;
            quoted = true;
        } else if quote.is_none() && matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            quoted = true;
        } else if byte == b'\\' && quote != Some(b'\'') {
            quoted = true;
            pos += 1;
            if let Some(&next) = word.raw.get(pos) {
                if quote == Some(b'"') && !b"$`\"\\\n".contains(&next) {
                    result.push(b'\\');
                }
                if next != b'\n' {
                    result.push(next);
                }
            }
        } else if byte == b'$' && word.raw.get(pos + 1) == Some(&b'\'') && quote.is_none() {
            return Err(error(
                ParseErrorKind::Unsupported,
                word.span.start,
                "ANSI-C quoted here-document delimiter is unsupported",
            ));
        } else {
            result.push(byte);
        }
        pos += 1;
    }
    Ok((result, !quoted))
}

fn fill_heredocs(list: &mut List, docs: &[Vec<u8>]) {
    for item in list {
        fill_pipeline(&mut item.chain.first, docs);
        for (_, pipeline) in &mut item.chain.rest {
            fill_pipeline(pipeline, docs);
        }
    }
}

fn fill_pipeline(pipeline: &mut Pipeline, docs: &[Vec<u8>]) {
    for command in &mut pipeline.commands {
        fill_command(command, docs);
    }
}

fn fill_command(command: &mut Command, docs: &[Vec<u8>]) {
    for redirect in &mut command.redirects {
        if let Some(doc) = &mut redirect.heredoc {
            doc.body.clone_from(&docs[doc.id]);
        }
    }
    match &mut command.kind {
        CommandKind::Group(body) | CommandKind::Subshell(body) | CommandKind::For { body, .. } => {
            fill_heredocs(body, docs);
        }
        CommandKind::If {
            branches,
            else_body,
        } => {
            for (condition, body) in branches {
                fill_heredocs(condition, docs);
                fill_heredocs(body, docs);
            }
            fill_heredocs(else_body, docs);
        }
        CommandKind::While {
            condition, body, ..
        } => {
            fill_heredocs(condition, docs);
            fill_heredocs(body, docs);
        }
        CommandKind::Case { arms, .. } => {
            for arm in arms {
                fill_heredocs(&mut arm.body, docs);
            }
        }
        CommandKind::Function { body, .. } => fill_command(body, docs),
        CommandKind::Simple(_) | CommandKind::Arithmetic(_) | CommandKind::Conditional(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple(command: &Command) -> &[Word] {
        let CommandKind::Simple(words) = &command.kind else {
            panic!("expected simple command")
        };
        words
    }

    #[test]
    fn one_unit_does_not_parse_or_execute_the_next() {
        let input = b"echo first; echo second\nif false; then\n";
        let unit = parse_one(input).unwrap();
        assert_eq!(unit.consumed, 24);
        assert_eq!(unit.body.len(), 2);
        assert_eq!(check(&input[unit.consumed..]), SyntaxStatus::Incomplete);
        assert_eq!(parse_one(b"echo valid\n)").unwrap().consumed, 11);
    }

    #[test]
    fn preserves_word_bytes_quotes_and_spans() {
        let input = b"git -C \"$repo\" switch 'caf\xc3\xa9' a\\ b raw\xff\n";
        let unit = parse_one(input).unwrap();
        for word in simple(&unit.body[0].chain.first.commands[0]) {
            assert_eq!(&input[word.span.clone()], word.raw);
        }
        assert_eq!(
            simple(&unit.body[0].chain.first.commands[0])[2].raw,
            b"\"$repo\""
        );
    }

    #[test]
    fn pipeline_and_boolean_precedence() {
        let unit = parse_one(b"! false | cat && yes || no & next").unwrap();
        assert_eq!(unit.body.len(), 2);
        assert!(unit.body[0].background);
        assert!(unit.body[0].chain.first.negate);
        assert_eq!(unit.body[0].chain.first.commands.len(), 2);
        assert_eq!(unit.body[0].chain.rest[0].0, LogicalOp::And);
        assert_eq!(unit.body[0].chain.rest[1].0, LogicalOp::Or);
    }

    #[test]
    fn compound_forms_and_raw_arrays() {
        let input = b"f() { for x in a 'b c'; do if test -n \"$x\"; then echo \"$x\"; elif false; then :; else :; fi; done; }; (while false; do :; done); a=(one 'two three' [4]=four)";
        let unit = parse_one(input).unwrap();
        assert_eq!(unit.body.len(), 3);
        assert!(matches!(
            unit.body[0].chain.first.commands[0].kind,
            CommandKind::Function { .. }
        ));
        assert!(matches!(
            unit.body[1].chain.first.commands[0].kind,
            CommandKind::Subshell(_)
        ));
        assert_eq!(
            simple(&unit.body[2].chain.first.commands[0])[0].raw,
            b"a=(one 'two three' [4]=four)"
        );
    }

    #[test]
    fn case_and_nested_command_substitutions() {
        let input =
            b"echo \"$(case x in (x|y) printf '%s' \"${v:-$(echo nested)}\";; *) :;; esac)\"";
        assert_eq!(check(input), SyntaxStatus::Complete);
        let start = input.windows(2).position(|part| part == b"$(").unwrap();
        assert_eq!(expansion_end(input, start).unwrap(), input.len() - 1);
    }

    #[test]
    fn heredoc_bodies_are_not_shell_commands() {
        let input =
            b"cat <<'EOF' 3<<-END\n$(touch /never-run)\nEOF\n\tsecond\n\tEND\necho following\n";
        let unit = parse_one(input).unwrap();
        let redirects = &unit.body[0].chain.first.commands[0].redirects;
        assert_eq!(
            redirects[0].heredoc.as_ref().unwrap().body,
            b"$(touch /never-run)\n"
        );
        assert!(!redirects[0].heredoc.as_ref().unwrap().expand);
        assert_eq!(redirects[1].fd, Some(3));
        assert_eq!(redirects[1].heredoc.as_ref().unwrap().body, b"second\n");
        assert_eq!(&input[unit.consumed..], b"echo following\n");
    }

    #[test]
    fn completeness_distinguishes_errors_and_unsupported_forms() {
        for input in [
            b"echo '".as_slice(),
            b"echo \\",
            b"a |",
            b"a &&\n",
            b"if true; then :",
            b"for x in a; do :",
            b"echo $(case x in x) :;;",
            b"cat <<EOF\nbody\n",
        ] {
            assert_eq!(
                check(input),
                SyntaxStatus::Incomplete,
                "{}",
                String::from_utf8_lossy(input)
            );
        }
        for input in [
            b")".as_slice(),
            b"echo x ;;",
            b"if ; then :; fi",
            b"{ }",
            b"[[ -n ]]",
            b"[[ x == ]]",
            b"[[ x && ]]",
        ] {
            assert!(
                matches!(
                    check(input),
                    SyntaxStatus::Invalid(ParseError {
                        kind: ParseErrorKind::Invalid,
                        ..
                    })
                ),
                "{}",
                String::from_utf8_lossy(input)
            );
        }
        for input in [
            b"cat <(echo x)".as_slice(),
            b"echo x |& cat",
            b"for ((i=0;i<2;i++)); do :; done",
        ] {
            assert!(
                matches!(
                    check(input),
                    SyntaxStatus::Invalid(ParseError {
                        kind: ParseErrorKind::Unsupported,
                        ..
                    })
                ),
                "{}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn aliases_expand_syntax_without_losing_original_consumption() {
        let aliases = BTreeMap::from([
            (b"p".to_vec(), b"printf '%s' hello | cat".to_vec()),
            (b"a".to_vec(), b"p".to_vec()),
            (b"ls".to_vec(), b"ls --color".to_vec()),
            (b"prefix".to_vec(), b"echo ".to_vec()),
            (b"value".to_vec(), b"expanded".to_vec()),
            (b"err".to_vec(), b"echo 2>&1".to_vec()),
            (b"line".to_vec(), b"echo line\n".to_vec()),
            (b"empty".to_vec(), Vec::new()),
            (b"end".to_vec(), b"fi".to_vec()),
        ]);
        let unit = parse_one_with_aliases(b"a\nsecond", &aliases).unwrap();
        assert_eq!(unit.consumed, 2);
        assert_eq!(unit.body[0].chain.first.commands.len(), 2);
        assert_eq!(
            simple(&unit.body[0].chain.first.commands[0])[0].raw,
            b"printf"
        );
        assert!(parse_one_with_aliases(b"ls", &aliases).is_ok());
        let unit = parse_one_with_aliases(b"x=1 prefix value", &aliases).unwrap();
        assert_eq!(
            simple(&unit.body[0].chain.first.commands[0])
                .iter()
                .map(|w| w.raw.clone())
                .collect::<Vec<_>>(),
            [b"x=1".to_vec(), b"echo".to_vec(), b"expanded".to_vec()]
        );
        let unit = parse_one_with_aliases(b"err", &aliases).unwrap();
        assert_eq!(
            unit.body[0].chain.first.commands[0].redirects[0].fd,
            Some(2)
        );
        assert_eq!(
            parse_one_with_aliases(b"line\nif", &aliases)
                .unwrap()
                .consumed,
            5
        );
        let unit = parse_one_with_aliases(b"empty\n", &aliases).unwrap();
        assert!(unit.body.is_empty());
        assert_eq!(unit.consumed, 6);
        assert!(parse_one_with_aliases(b"empty; echo x", &aliases).is_err());
        assert!(parse_one_with_aliases(b"if true; then :; end", &aliases).is_ok());
    }

    #[test]
    fn conditional_regular_expression_is_one_raw_word() {
        let unit = parse_one(b"[[ value =~ ^(foo|bar(a b))$|other && x == y ]]").unwrap();
        let CommandKind::Conditional(words) = &unit.body[0].chain.first.commands[0].kind else {
            panic!("expected conditional");
        };
        assert_eq!(words[2].raw, b"^(foo|bar(a b))$|other");
        assert_eq!(words[2].span, 12..34);
        assert_eq!(check(b"[[ value =~ (foo ]]"), SyntaxStatus::Incomplete);
    }

    #[test]
    fn extended_patterns_keep_their_balanced_source() {
        let unit = parse_one(b"case item in @(item|other)) :;; !(foo|+(bar))) :;; esac").unwrap();
        let CommandKind::Case { arms, .. } = &unit.body[0].chain.first.commands[0].kind else {
            panic!("expected case");
        };
        assert_eq!(arms[0].patterns[0].raw, b"@(item|other)");
        assert_eq!(arms[1].patterns[0].raw, b"!(foo|+(bar))");
        let unit = parse_one(b"echo prefix?(one|two)*.txt").unwrap();
        assert_eq!(
            simple(&unit.body[0].chain.first.commands[0])[1].raw,
            b"prefix?(one|two)*.txt"
        );
        assert_eq!(check(b"echo @(unfinished"), SyntaxStatus::Incomplete);
    }

    #[test]
    fn redirection_order_and_condition_words() {
        let unit = parse_one(b"echo word 2>&1 >out; [[ -n \"$v\" && x < y ]]; ((x += 2))").unwrap();
        let redirects = &unit.body[0].chain.first.commands[0].redirects;
        assert_eq!(redirects[0].fd, Some(2));
        assert_eq!(redirects[0].kind, RedirectKind::DupWrite);
        assert_eq!(redirects[1].kind, RedirectKind::Write);
        assert!(matches!(
            unit.body[1].chain.first.commands[0].kind,
            CommandKind::Conditional(_)
        ));
        assert!(matches!(
            unit.body[2].chain.first.commands[0].kind,
            CommandKind::Arithmetic(_)
        ));
    }

    #[test]
    fn excessive_nested_expansions_fail_without_recursing_indefinitely() {
        let input = [
            b"echo ".to_vec(),
            b"${x:-".repeat(200),
            b"x".to_vec(),
            b"}".repeat(200),
        ]
        .concat();
        assert!(matches!(
            check(&input),
            SyntaxStatus::Invalid(ParseError {
                kind: ParseErrorKind::Unsupported,
                ..
            })
        ));
        let input = [
            b"echo ".to_vec(),
            b"$(echo ".repeat(200),
            b"x".to_vec(),
            b")".repeat(200),
        ]
        .concat();
        assert!(matches!(
            check(&input),
            SyntaxStatus::Invalid(ParseError {
                kind: ParseErrorKind::Unsupported,
                ..
            })
        ));
    }
}
