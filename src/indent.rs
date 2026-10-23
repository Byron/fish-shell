//! Shared AST indentation for the formatter and both shell frontends.
use crate::{
    ast::{self, Kind, Leaf as _, Node, NodeVisitor},
    commandline_extent::locate_cmdsubst_range,
    parse_constants::{ParseTokenType, ParseTreeFlags, SourceRange},
    tokenizer::{TokFlags, Tokenizer, quote_end},
};
use fish_widestring::{WExt as _, WString, wstr};
use std::ops::Range;

/// Given a string, parse it as fish code and then return the indents. The return value has the same
/// size as the string.
pub fn compute_indents(src: &wstr) -> Vec<i32> {
    compute_indents_from(src, 0, false)
}

/// The formatter can introduce newlines into a single-line block.
pub(crate) fn compute_indents_for_formatter(src: &wstr) -> Vec<i32> {
    compute_indents_from(src, 0, true)
}

/// Compute the indents for the given source, starting with the specified initial indent.
fn compute_indents_from(src: &wstr, initial_indent: i32, format: bool) -> Vec<i32> {
    // Make a vector the same size as the input string, which contains the indents. Initialize them
    // to 0.
    let mut indents = vec![0; src.len()];

    // Simple trick: if our source does not contain a newline, then all indents are 0.
    if !format && !src.chars().any(|c| c == '\n') {
        return indents;
    }

    // Parse the string. We pass continue_after_error to produce a forest; the trailing indent of
    // the last node we visited becomes the input indent of the next. I.e. in the case of 'switch
    // foo ; cas', we get an invalid parse tree (since 'cas' is not valid) but we indent it as if it
    // were a case item list.
    let flags = ParseTreeFlags {
        continue_after_error: true,
        include_comments: true,
        accept_incomplete_tokens: true,
        leave_unterminated: true,
        ..Default::default()
    };
    let ast = ast::parse(src, flags, None);
    {
        let mut iv = IndentVisitor::new(src, &mut indents, initial_indent);
        iv.visit(ast.top());
        iv.record_line_continuations_until(iv.indents.len());
        iv.indents[iv.last_leaf_end..].fill(iv.last_indent);

        // All newlines now get the *next* indent.
        // For example, in this code:
        //    if true
        //       stuff
        // the newline "belongs" to the if statement as it ends its job.
        // But when rendered, it visually belongs to the job list.

        let mut idx = src.len();
        let mut next_indent = iv.last_indent;
        let src = src.as_char_slice();
        while idx != 0 {
            idx -= 1;
            if src[idx] == '\n' {
                let empty_middle_line = src.get(idx + 1) == Some(&'\n');
                let is_trailing_unclosed = idx == src.len() - 1 && iv.unclosed;
                if !empty_middle_line && !is_trailing_unclosed {
                    iv.indents[idx] = next_indent;
                }
            } else {
                next_indent = iv.indents[idx];
            }
        }
        // Add an extra level of indentation to continuation lines.
        for mut idx in iv.line_continuations {
            loop {
                indents[idx] = indents[idx].wrapping_add(1);
                idx += 1;
                if idx == src.len() || src[idx] == '\n' {
                    break;
                }
            }
        }
    }

    indents
}

// The number of spaces per indent isn't supposed to be configurable.
// See discussion at https://github.com/fish-shell/fish-shell/pull/6790
pub const SPACES_PER_INDENT: usize = 4;

pub fn apply_indents(src: &wstr, indents: &[i32]) -> WString {
    let mut indented = WString::new();
    for (i, c) in src.chars().enumerate() {
        indented.push(c);
        if c != '\n' || i + 1 == src.len() {
            continue;
        }
        indented.extend(std::iter::repeat_n(
            ' ',
            SPACES_PER_INDENT * usize::try_from(indents[i]).unwrap(),
        ));
    }
    indented
}

// Visit all of our nodes. When we get a job_list or case_item_list, increment indent while
// visiting its children.
struct IndentVisitor<'a> {
    // The parent node of the node we are currently visiting, or None if we are the root.
    parent: Option<&'a dyn ast::Node>,

    // companion: Pin<&'a mut IndentVisitor>,
    // The one-past-the-last index of the most recently encountered leaf node.
    // We use this to populate the indents even if there's no tokens in the range.
    last_leaf_end: usize,

    // The last indent which we assigned.
    last_indent: i32,

    // Whether we have an unfinished quote or command substitution.
    unclosed: bool,

    // The source we are indenting.
    src: &'a wstr,

    // List of indents, which we populate.
    indents: &'a mut Vec<i32>,

    // Initialize our starting indent to -1, as our top-level node is a job list which
    // will immediately increment it.
    indent: i32,

    // List of locations of escaped newline characters.
    line_continuations: Vec<usize>,
}

impl<'a> IndentVisitor<'a> {
    fn new(src: &'a wstr, indents: &'a mut Vec<i32>, initial_indent: i32) -> Self {
        Self {
            parent: None,
            last_leaf_end: 0,
            last_indent: initial_indent - 1,
            unclosed: false,
            src,
            indents,
            indent: initial_indent - 1,
            line_continuations: vec![],
        }
    }
    /// Return whether a maybe_newlines node contains at least one newline.
    fn has_newline(&self, nls: &ast::MaybeNewlines) -> bool {
        nls.source(self.src).chars().any(|c| c == '\n')
    }
    fn record_line_continuations_until(&mut self, offset: usize) {
        let gap_text = &self.src[self.last_leaf_end..offset];
        let gap_text = gap_text.as_char_slice();
        let Some((escaped_nl, length)) = gap_text.iter().enumerate().find_map(|(pos, _)| {
            let length = fish_wcstringutil::escape::line_continuation_length(&gap_text[pos..]);
            (length > 0).then_some((pos, length))
        }) else {
            return;
        };
        if gap_text[..escaped_nl].contains(&'#') {
            return;
        }
        let mut newline = escaped_nl + length - 1;
        // The gap text might contain multiple newlines if there are multiple lines that
        // don't contain an AST node, for example, comment lines, or lines containing only
        // the escaped newline.
        loop {
            self.line_continuations.push(self.last_leaf_end + newline);
            match gap_text[newline + 1..].iter().position(|c| *c == '\n') {
                Some(nextnl) => newline = newline + 1 + nextnl,
                None => break,
            }
        }
    }

    fn indent_leaf(&mut self, range: SourceRange) {
        let node_src = &self.src[range.start()..range.end()];
        // Common case optimization.
        if node_src.contains('(') /*)*/ && !node_src.contains('\n') {
            self.indents[range.start()..range.end()].fill(self.indent);
            return;
        }

        let mut done = range.start();
        let mut cursor = 0;
        let mut is_double_quoted = false;
        let mut was_double_quoted;
        loop {
            was_double_quoted = is_double_quoted;
            let cmdsub = match locate_cmdsubst_range(
                node_src,
                &mut cursor,
                /*accept_incomplete=*/ true,
                Some(&mut is_double_quoted),
                None,
            ) {
                Err(()) => break,
                Ok(None) => {
                    break;
                }
                Ok(Some(cmdsub)) => cmdsub,
            };

            let command = cmdsub.command_range();
            self.indent_string_part(done..range.start() + command.start, was_double_quoted);
            let cmdsub_contents = &node_src[command.clone()];
            let indents = compute_indents_from(cmdsub_contents, self.indent + 1, false);
            self.indents[range.start() + command.start..range.start() + command.end]
                .copy_from_slice(&indents);

            done = range.start() + command.end;
            if cmdsub.closing_paren_range().is_empty() {
                self.unclosed = true;
            }
        }
        self.indent_string_part(done..range.end(), was_double_quoted);
    }

    fn indent_string_part(&mut self, range: Range<usize>, is_double_quoted: bool) {
        let mut start = range.start;
        let mut quoted = false;
        if is_double_quoted {
            match quote_end(self.src, range.start, '"') {
                Some(q_end) => {
                    // We may be (in) a multi-line string, so don't indent.
                    start = q_end + 1;
                }
                None => quoted = true,
            }
        }
        let mut done = start;
        if !quoted {
            let part = &self.src[done..range.end];
            let mut callback = |offset| {
                if !quoted {
                    // Quote open event. Indent unquoted part, including the opening quote.
                    self.indents[done..=(start + offset)].fill(self.indent);
                    done = start + offset + 1;
                } else {
                    // Quote close. Don't indent, in case it's a multiline string.
                    // Mark the first line as indented but only to make tests look prettier.
                    let first_line_length = self.src[start..start + offset]
                        .chars()
                        .take_while(|&c| c != '\n')
                        .count();
                    self.indents[start..start + first_line_length].fill(self.indent);
                    done = start + offset;
                }
                quoted = !quoted;
            };
            for _token in Tokenizer::with_quote_events(
                part,
                TokFlags {
                    accept_unfinished: true,
                    ..Default::default()
                },
                &mut callback,
            ) {}
        }
        if !quoted {
            self.indents[done..range.end].fill(self.indent);
        } else {
            self.unclosed = true;
        }
    }
}

impl<'a> NodeVisitor<'a> for IndentVisitor<'a> {
    // Default implementation is to just visit children.
    fn visit(&mut self, node: &'a dyn Node) {
        let mut inc_dec = (0, 0);
        match node.kind() {
            Kind::JobList(_) | Kind::AndorJobList(_) => {
                // Job lists are never unwound.
                inc_dec = (1, 1);
            }

            // Increment indents for conditions in headers (#1665).
            Kind::JobConjunction(_node) => {
                let parent_kind = self.parent.unwrap().kind();
                if matches!(parent_kind, Kind::IfClause(_) | Kind::WhileHeader(_)) {
                    inc_dec = (1, 1);
                }
            }

            // Increment indents for JobContinuation if it contains a newline.
            // This is a bit of a hack - it indents cases like:
            //    cmd1 |
            //    ....cmd2
            // but avoids "double indenting" if there's no newline:
            //   cmd1 | while cmd2
            //   ....cmd3
            //   end
            // See #7252.
            Kind::JobContinuation(node) if self.has_newline(&node.newlines) => {
                inc_dec = (1, 1);
            }

            // Likewise for && and ||.
            Kind::JobConjunctionContinuation(node) if self.has_newline(&node.newlines) => {
                inc_dec = (1, 1);
            }

            Kind::CaseItemList(_) => {
                // Here's a hack. Consider:
                // switch abc
                //    cas
                //
                // fish will see that 'cas' is not valid inside a switch statement because it is
                // not "case". It will then unwind back to the top level job list, producing a
                // parse tree like:
                //
                //   job_list
                //      switch_job
                //         <err>
                //      normal_job
                //         cas
                //
                // And so we will think that the 'cas' job is at the same level as the switch.
                // To address this, if we see that the switch statement was not closed, do not
                // decrement the indent afterwards.
                let Kind::SwitchStatement(switchs) = self.parent.unwrap().kind() else {
                    panic!("Expected switch statement");
                };
                let dec = if switchs.end.has_source() { 1 } else { 0 };
                inc_dec = (1, dec);
            }

            Kind::Token(node) => {
                let token_type = node.token_type();
                let parent_kind = self.parent.unwrap().kind();
                if matches!(parent_kind, Kind::BeginHeader(_)) && token_type == ParseTokenType::End
                {
                    // The newline after "begin" is optional, so it is part of the header.
                    // The header is not in the indented block, so indent the newline here.
                    if node.source(self.src) == "\n" {
                        inc_dec = (1, 1);
                    }
                }
            }

            _ => {}
        }
        let range = node.source_range();
        if range.length() > 0 && node.as_leaf().is_some() {
            self.record_line_continuations_until(range.start());
            self.indents[self.last_leaf_end..range.start()].fill(self.last_indent);
        }

        self.indent += inc_dec.0;

        // If we increased the indentation, apply it to the remainder of the string, even if the
        // list is empty. For example (where _ represents the cursor):
        //
        //    if foo
        //       _
        //
        // we want to indent the newline.
        if inc_dec.0 != 0 {
            self.last_indent = self.indent;
        }

        // If this is a leaf node, apply the current indentation.
        if node.as_leaf().is_some() && range.length() != 0 {
            let leading_spaces = self.src[..range.start()]
                .chars()
                .rev()
                .take_while(|&c| c == ' ')
                .count();
            self.indents[range.start() - leading_spaces..range.start()].fill(self.indent);
            self.indent_leaf(range);
            self.last_leaf_end = range.end();
            self.last_indent = self.indent;
        }

        let saved = self.parent.replace(node);
        node.accept(self);
        self.parent = saved;
        self.indent -= inc_dec.1;
    }
}

/// Remove the shared indentation only when every nonempty following line has it.
pub(crate) fn unindent(source: &wstr, indents: &[i32]) -> WString {
    for (index, character) in source.chars().enumerate() {
        if character != '\n' || index + 1 == source.len() {
            continue;
        }
        let spaces = SPACES_PER_INDENT * usize::try_from(indents[index + 1]).unwrap();
        if source.len() < index + 1 + spaces
            || !source[index + 1..].chars().take(spaces).all(|c| c == ' ')
        {
            return source.to_owned();
        }
    }
    let mut output = WString::new();
    let mut index = 0;
    while index < source.len() {
        let character = source.char_at(index);
        output.push(character);
        index += 1;
        if character == '\n' && index < source.len() {
            index += SPACES_PER_INDENT * usize::try_from(indents[index]).unwrap();
        }
    }
    output
}
