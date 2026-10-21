//! Programmatic representation of fish code.

use std::ops::Deref;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::Arc;

use crate::ast::{self, Ast, JobList, Node};
use crate::parse_constants::{ParseErrorList, ParseTreeFlags};
use crate::prelude::*;
use fish_common::{assert_send, assert_sync};
use fish_wcstringutil::count_newlines;

#[path = "parse_token.rs"]
mod parse_token;
pub use parse_token::ParseToken;

/// A type wrapping up a parse tree and the original source behind it.
pub struct ParsedSource {
    pub src: WString,
    pub ast: Ast,
}

const _: () = assert_send::<ParsedSource>();
const _: () = assert_sync::<ParsedSource>();

/// Cache for efficient line number computation when processing multiple nodes.
/// Caches the last source's offset and newline count.
#[derive(Default)]
pub struct SourceLineCache {
    /// Pointer to last source used
    last_source: Option<NonNull<ParsedSource>>,
    /// Exclusive offset of the number of newlines counted
    offset: usize,
    /// Count of newlines up to offset
    count: usize,
}

impl ParsedSource {
    /// Compute the 1-based line number for a given offset, using and updating the cache.
    pub fn lineno_for_offset(&self, offset: usize, cache: &mut SourceLineCache) -> u32 {
        // TODO(MSRV>=1.89): feature(non_null_from_ref)
        let self_ptr = unsafe { NonNull::new_unchecked(std::ptr::from_ref(self).cast_mut()) };

        // If source changed, reset cache.
        if cache.last_source != Some(self_ptr) {
            cache.last_source = Some(self_ptr);
            cache.offset = 0;
            cache.count = 0;
        }

        #[allow(clippy::comparison_chain)] // TODO(MSRV>=1.90) old clippy
        if offset > cache.offset {
            cache.count += count_newlines(&self.src[cache.offset..offset]);
        } else if offset < cache.offset {
            cache.count -= count_newlines(&self.src[offset..cache.offset]);
        }
        cache.offset = offset;

        cache.count as u32 + 1
    }
}

impl ParsedSource {
    pub fn new(src: WString, ast: Ast) -> Self {
        ParsedSource { src, ast }
    }

    // Return the top NodeRef for the parse tree, which is of type JobList.
    pub fn top_job_list(self: &Arc<Self>) -> NodeRef<JobList> {
        // SAFETY: The top node is owned by this parsed source.
        unsafe { NodeRef::new(Arc::clone(self), self.ast.top()) }
    }
}

pub type ParsedSourceRef = Arc<ParsedSource>;

/// A reference to a node within a parse tree.
pub struct NodeRef<NodeType: Node> {
    /// The parse tree containing the node.
    /// This is pinned because we hold a pointer into it.
    parsed_source: Pin<Arc<ParsedSource>>,

    /// The node itself. This points into the parsed source.
    node: *const NodeType,
}

impl<NodeType: Node> NodeRef<NodeType> {
    /// # Safety
    /// `node` must reference a node owned by `parsed_source`.
    pub unsafe fn new(parsed_source: ParsedSourceRef, node: &NodeType) -> Self {
        NodeRef {
            parsed_source: Pin::new(parsed_source),
            node,
        }
    }

    // Given a NodeRef, map to a child of the Node.
    // The caller provides a closure to return a reference to the child node
    // from the parent.
    pub fn child_ref<ChildType: Node>(
        &self,
        func: impl FnOnce(&NodeType) -> &ChildType,
    ) -> NodeRef<ChildType> {
        NodeRef {
            parsed_source: self.parsed_source.clone(),
            node: func(self),
        }
    }

    // Return the source offset of this node, if any.
    pub fn source_offset(&self) -> Option<usize> {
        self.try_source_range().map(|r| r.start())
    }

    // Return the source, as a string.
    pub fn source_str(&self) -> &wstr {
        &self.parsed_source.src
    }
}

impl<NodeType: Node> Clone for NodeRef<NodeType> {
    fn clone(&self) -> Self {
        NodeRef {
            parsed_source: self.parsed_source.clone(),
            node: self.node,
        }
    }
}

impl<NodeType: Node> Deref for NodeRef<NodeType> {
    type Target = NodeType;
    fn deref(&self) -> &Self::Target {
        // Safety: the node is valid for the lifetime of the source.
        unsafe { &*self.node }
    }
}

impl<NodeType: Node> NodeRef<NodeType> {
    pub fn parsed_source(&self) -> &ParsedSource {
        &self.parsed_source
    }

    pub fn parsed_source_ref(&self) -> ParsedSourceRef {
        Pin::into_inner(self.parsed_source.clone())
    }

    /// Return the 1-based line number of this node, or None if unsourced.
    pub fn lineno(&self) -> Option<std::num::NonZeroU32> {
        self.lineno_with_cache(&mut SourceLineCache::default())
    }

    /// Return the 1-based line number of this node using a cache, or None if unsourced.
    pub fn lineno_with_cache(&self, cache: &mut SourceLineCache) -> Option<std::num::NonZeroU32> {
        let range = self.try_source_range()?;
        let lineno = self.parsed_source.lineno_for_offset(range.start(), cache);
        Some(std::num::NonZeroU32::new(lineno).unwrap())
    }
}

// Safety: NodeRef is Send and Sync because it's just a pointer into a parse tree, which is pinned.
unsafe impl<NodeType: Node> Send for NodeRef<NodeType> {}
unsafe impl<NodeType: Node> Sync for NodeRef<NodeType> {}

/// Return a shared pointer to ParsedSource, or null on failure.
/// If parse_flag_continue_after_error is not set, this will return null on any error.
pub fn parse_source(
    src: WString,
    flags: ParseTreeFlags,
    errors: Option<&mut ParseErrorList>,
) -> Option<ParsedSourceRef> {
    let ast = ast::parse(&src, flags, errors);
    if ast.errored() && !flags.continue_after_error {
        None
    } else {
        Some(Arc::new(ParsedSource::new(src, ast)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lineno_for_offset() {
        let src = L!("a\nb\nc\nd").to_owned();
        let ps = ParsedSource::new(src, ast::parse(L!(""), ParseTreeFlags::default(), None));
        let mut cache = SourceLineCache::default();

        // Forward progression
        assert_eq!(ps.lineno_for_offset(0, &mut cache), 1);
        assert_eq!(ps.lineno_for_offset(4, &mut cache), 3);
        assert_eq!(ps.lineno_for_offset(6, &mut cache), 4);

        // Backward progression
        assert_eq!(ps.lineno_for_offset(2, &mut cache), 2);
        assert_eq!(ps.lineno_for_offset(0, &mut cache), 1);
    }
}
