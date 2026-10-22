//! Output boundaries shared by command substitutions on every platform.
/// separated_buffer_t represents a buffer of output from commands, prepared to be turned into a
/// variable. For example, command substitutions output into one of these. Most commands just
/// produce a stream of bytes, and those get stored directly. However other commands produce
/// explicitly separated output, in particular `string` like `string collect` and `string split0`.
/// The buffer tracks a sequence of elements. Some elements are explicitly separated and should not
/// be further split; other elements have inferred separation and may be split by IFS (or not,
/// depending on its value).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeparationType {
    /// this element should be further separated by IFS
    Inferred,
    /// this element is explicitly separated and should not be further split
    Explicitly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BufferElement {
    pub contents: Vec<u8>,
    pub separation: SeparationType,
}

impl BufferElement {
    pub fn new(contents: Vec<u8>, separation: SeparationType) -> Self {
        BufferElement {
            contents,
            separation,
        }
    }
    pub fn is_explicitly_separated(&self) -> bool {
        self.separation == SeparationType::Explicitly
    }
}

/// A separated_buffer_t contains a list of elements, some of which may be separated explicitly and
/// others which must be separated further by the user (e.g. via IFS).
#[derive(Clone, Debug)]
pub struct SeparatedBuffer {
    /// Limit on how much data we'll buffer. Zero means no limit.
    buffer_limit: usize,
    /// Current size of all contents.
    contents_size: usize,
    /// List of buffer elements.
    elements: Vec<BufferElement>,
    /// True if we're discarding input because our buffer_limit has been exceeded.
    discard: bool,
}

impl SeparatedBuffer {
    pub fn new(limit: usize) -> Self {
        SeparatedBuffer {
            buffer_limit: limit,
            contents_size: 0,
            elements: vec![],
            discard: false,
        }
    }

    /// Return the buffer limit size, or 0 for no limit.
    pub fn limit(&self) -> usize {
        self.buffer_limit
    }

    /// Return the contents size.
    pub fn len(&self) -> usize {
        self.contents_size
    }

    /// Return whether the output has been discarded.
    pub fn discarded(&self) -> bool {
        self.discard
    }

    /// Serialize the contents to a single string, where explicitly separated elements have a
    /// newline appended.
    pub fn newline_serialized(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(self.len());
        for elem in &self.elements {
            result.extend_from_slice(&elem.contents);
            if elem.is_explicitly_separated() {
                result.push(b'\n');
            }
        }
        result
    }

    /// Return the list of elements.
    pub fn elements(&self) -> &[BufferElement] {
        &self.elements
    }

    /// Append the given data with separation type `sep`.
    pub fn append(&mut self, data: &[u8], sep: SeparationType) -> bool {
        if !self.try_add_size(data.len()) {
            return false;
        }
        // Try merging with the last element.
        if sep == SeparationType::Inferred && self.last_inferred() {
            self.elements
                .last_mut()
                .unwrap()
                .contents
                .extend_from_slice(data);
        } else {
            self.elements.push(BufferElement::new(data.to_vec(), sep));
        }
        true
    }

    /// Remove all elements and unset the discard flag.
    pub fn clear(&mut self) {
        self.elements.clear();
        self.contents_size = 0;
        self.discard = false;
    }

    /// Return true if our last element has an inferred separation type.
    fn last_inferred(&self) -> bool {
        !self.elements.is_empty() && !self.elements.last().unwrap().is_explicitly_separated()
    }

    /// Mark that we are about to add the given size `delta` to the buffer. Return true if we
    /// succeed, false if we exceed buffer_limit.
    fn try_add_size(&mut self, delta: usize) -> bool {
        if self.discard {
            return false;
        }
        let proposed_size = self.contents_size + delta;
        if proposed_size < delta || (self.buffer_limit > 0 && proposed_size > self.buffer_limit) {
            self.clear();
            self.discard = true;
            return false;
        }
        self.contents_size = proposed_size;
        true
    }
}
