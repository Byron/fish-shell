//! Platform-independent tokens shared by the Fish parser and its execution backends.
use crate::parse_constants::{
    ParseErrorCode, ParseKeyword, ParseTokenType, SOURCE_OFFSET_INVALID, SourceOffset, SourceRange,
    token_type_user_presentable_description,
};
use crate::prelude::*;
use crate::tokenizer::TokenizerError;

/// A struct representing the token type that we use internally.
#[derive(Clone, Copy)]
pub struct ParseToken {
    /// The type of the token as represented by the parser
    pub typ: ParseTokenType,
    /// Any keyword represented by this token
    pub keyword: ParseKeyword,
    /// Hackish: whether the source contains a dash prefix
    pub has_dash_prefix: bool,
    /// Hackish: whether the source looks like '-h' or '--help'
    pub is_help_argument: bool,
    /// Hackish: whether the source is exactly '--'.
    pub is_double_dash: bool,
    /// Hackish: if `TokenType::End`, whether the source is a newline.
    pub is_newline: bool,
    // Hackish: whether this token is a string like FOO=bar
    pub may_be_variable_assignment: bool,
    /// If this is a tokenizer error, that error.
    pub tok_error: TokenizerError,
    source_start: SourceOffset,
    source_length: SourceOffset,
}

impl ParseToken {
    pub fn new(typ: ParseTokenType) -> Self {
        ParseToken {
            typ,
            keyword: ParseKeyword::None,
            has_dash_prefix: false,
            is_help_argument: false,
            is_double_dash: false,
            is_newline: false,
            may_be_variable_assignment: false,
            tok_error: TokenizerError::None,
            source_start: SOURCE_OFFSET_INVALID.try_into().unwrap(),
            source_length: 0,
        }
    }
    pub fn set_source_start(&mut self, value: usize) {
        self.source_start = value.try_into().unwrap();
    }
    pub fn source_start(&self) -> usize {
        self.source_start.try_into().unwrap()
    }
    pub fn set_source_length(&mut self, value: usize) {
        self.source_length = value.try_into().unwrap();
    }
    pub fn source_length(&self) -> usize {
        self.source_length.try_into().unwrap()
    }
    /// Return the source range.
    /// Note the start may be invalid.
    pub fn range(&self) -> SourceRange {
        SourceRange::new(self.source_start(), self.source_length())
    }
    /// Return whether we are a string with the dash prefix set.
    pub fn is_dash_prefix_string(&self) -> bool {
        self.typ == ParseTokenType::String && self.has_dash_prefix
    }
    /// Return whether we are a string that is exactly '--'.
    pub fn is_double_dash_string(&self) -> bool {
        self.typ == ParseTokenType::String && self.is_double_dash
    }
    /// Returns a string description of the given parse token.
    pub fn describe(&self) -> WString {
        let mut result = self.typ.to_wstr().to_owned();
        if self.keyword != ParseKeyword::None {
            sprintf!(=> &mut result, " <%s>", self.keyword.to_wstr());
        }
        result
    }
    pub fn user_presentable_description(&self) -> WString {
        token_type_user_presentable_description(self.typ, self.keyword)
    }
}

impl From<TokenizerError> for ParseErrorCode {
    fn from(err: TokenizerError) -> Self {
        match err {
            TokenizerError::None => ParseErrorCode::None,
            TokenizerError::UnterminatedQuote => ParseErrorCode::TokenizerUnterminatedQuote,
            TokenizerError::UnterminatedSubshell => ParseErrorCode::TokenizerUnterminatedSubshell,
            TokenizerError::UnterminatedSlice => ParseErrorCode::TokenizerUnterminatedSlice,
            TokenizerError::UnterminatedEscape => ParseErrorCode::TokenizerUnterminatedEscape,
            // To-do: maybe also unbalancing brace?
            _ => ParseErrorCode::TokenizerOther,
        }
    }
}
