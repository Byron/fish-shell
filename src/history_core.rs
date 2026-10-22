//! History records, searches, and file encoding shared by Fish's execution backends.
//!
//! Filesystem locking, session synchronization, and terminal presentation belong
//! to their respective backends; the record and search behavior is shared.

use fish_feature_flags::{FeatureFlag, feature_test};
use fish_wcstringutil::{subsequence_in_string, unescape_wildcards, wildcard_match};
use fish_widestring::{ANY_STRING, prelude::*, subslice_position, wcs2bytes};
use std::{
    borrow::Cow,
    time::{SystemTime, UNIX_EPOCH},
};

#[path = "history/yaml_backend.rs"]
pub(crate) mod yaml;

use yaml::{FIRST_ADDED_TIMESTAMP_KEY, LAST_ADDED_TIMESTAMP_KEY, escape_yaml_fish_2_0};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SearchType {
    /// Search for commands exactly matching the given string.
    Exact,
    /// Search for commands containing the given string.
    Contains,
    /// Search for commands starting with the given string.
    Prefix,
    /// Search for commands where any line matches the given string.
    LinePrefix,
    /// Search for commands containing the given glob pattern.
    ContainsGlob,
    /// Search for commands starting with the given glob pattern.
    PrefixGlob,
    /// Search for commands containing the given string as a subsequence
    ContainsSubsequence,
}

/// Ways that a history item may be written to disk (or omitted).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PersistenceMode {
    /// The history item is written to disk normally
    Disk,
    /// The history item is stored in-memory only, not written to disk
    Memory,
    /// The history item is stored in-memory and deleted when a new item is added
    Ephemeral,
}

pub type PathList = Vec<WString>;

#[derive(Clone, Debug)]
pub struct HistoryItem {
    /// The actual contents of the entry.
    pub(crate) contents: WString,
    /// Creation times of this history entry.
    pub(crate) timestamps: Timestamps,
    /// Paths that we require to be valid for this item to be autosuggested.
    pub(crate) required_paths: Vec<WString>,
    /// Whether to write this item to disk.
    pub(crate) persist_mode: PersistenceMode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timestamps {
    /// Time of most recent re-creation.
    pub(crate) last_added: SystemTime,
    /// Original creation time for the entry.
    pub(crate) first_added: SystemTime,
}

impl Timestamps {
    pub(crate) fn update_last_added(&mut self, new: Timestamps) {
        self.last_added = self.last_added.max(new.last_added);
    }
}

impl HistoryItem {
    /// Construct from a text, timestamp, and optional identifier.
    /// If `persist_mode` is not [`PersistenceMode::Disk`], then do not write this item to disk.
    pub(crate) fn new(
        s: WString,
        timestamps: Timestamps,
        persist_mode: PersistenceMode, /*=Disk*/
    ) -> Self {
        Self {
            contents: s,
            timestamps,
            required_paths: vec![],
            persist_mode,
        }
    }

    /// Returns the text as a string.
    pub fn str(&self) -> &wstr {
        &self.contents
    }

    /// Returns whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.contents.is_empty()
    }

    /// Returns whether our contents matches a search term.
    pub fn matches_search(&self, term: &wstr, typ: SearchType, case_sensitive: bool) -> bool {
        // Note that 'term' has already been lowercased when constructing the
        // search object if we're doing a case insensitive search.
        let content_to_match = if case_sensitive {
            Cow::Borrowed(&self.contents)
        } else {
            Cow::Owned(self.contents.to_lowercase())
        };

        match typ {
            SearchType::Exact => term == *content_to_match,
            SearchType::Contains => {
                subslice_position(content_to_match.as_slice(), term.as_slice()).is_some()
            }
            SearchType::Prefix => content_to_match.as_slice().starts_with(term.as_slice()),
            SearchType::LinePrefix => content_to_match
                .as_char_slice()
                .split(|&c| c == '\n')
                .any(|line| line.starts_with(term.as_char_slice())),
            SearchType::ContainsGlob => {
                let mut pat =
                    unescape_wildcards(term, !feature_test(FeatureFlag::QuestionMarkNoGlob));
                if !pat.starts_with(ANY_STRING) {
                    pat.insert(0, ANY_STRING);
                }
                if !pat.ends_with(ANY_STRING) {
                    pat.push(ANY_STRING);
                }
                wildcard_match(content_to_match.as_ref(), &pat, false)
            }
            SearchType::PrefixGlob => {
                let mut pat =
                    unescape_wildcards(term, !feature_test(FeatureFlag::QuestionMarkNoGlob));
                if !pat.ends_with(ANY_STRING) {
                    pat.push(ANY_STRING);
                }
                wildcard_match(content_to_match.as_ref(), &pat, false)
            }
            SearchType::ContainsSubsequence => subsequence_in_string(term, &content_to_match),
        }
    }

    /// Returns the timestamp of when this history item was last added.
    pub fn last_added_timestamp(&self) -> SystemTime {
        self.timestamps.last_added
    }

    /// Returns the timestamp for creating this history item.
    pub fn first_added_timestamp(&self) -> SystemTime {
        self.timestamps.first_added
    }

    /// Returns whether this item should be persisted (written to disk).
    pub fn should_write_to_disk(&self) -> bool {
        self.persist_mode == PersistenceMode::Disk
    }

    /// Get the list of arguments which referred to files.
    /// This is used for autosuggestion hinting.
    pub fn get_required_paths(&self) -> &[WString] {
        &self.required_paths
    }

    /// Set the list of arguments which referred to files.
    /// This is used for autosuggestion hinting.
    pub fn set_required_paths(&mut self, paths: Vec<WString>) {
        self.required_paths = paths;
    }

    /// We can merge two items if they are the same command. We use the more recent timestamp, more
    /// recent identifier, and the longer list of required paths.
    pub(crate) fn merge(&mut self, item: HistoryItem) -> Result<(), HistoryItem> {
        // We can only merge items if they agree on their text and persistence mode.
        if self.contents != item.contents || self.persist_mode != item.persist_mode {
            return Err(item);
        }

        // Ok, merge this item.
        self.timestamps.update_last_added(item.timestamps);
        if self.required_paths.len() < item.required_paths.len() {
            self.required_paths = item.required_paths;
        }
        Ok(())
    }
}

impl HistoryItem {
    /// Write this history item to some writer.
    pub fn write_to(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        assert!(self.should_write_to_disk(), "Item should not be persisted");

        let mut cmd = wcs2bytes(self.str());
        escape_yaml_fish_2_0(&mut cmd);
        writer.write_all(b"- cmd: ")?;
        writer.write_all(&cmd)?;
        writer.write_all(b"\n")?;
        let last_added = self.last_added_timestamp();
        let first_added = self.first_added_timestamp();
        writeln!(
            writer,
            "  {}: {}",
            LAST_ADDED_TIMESTAMP_KEY,
            time_to_seconds(last_added)
        )?;
        if first_added != last_added {
            writeln!(
                writer,
                "  {}: {}",
                FIRST_ADDED_TIMESTAMP_KEY,
                time_to_seconds(first_added)
            )?;
        }

        let paths = self.get_required_paths();
        if !paths.is_empty() {
            writeln!(writer, "  paths:")?;
            for path in paths {
                let mut path = wcs2bytes(path);
                escape_yaml_fish_2_0(&mut path);
                writer.write_all(b"    - ")?;
                writer.write_all(&path)?;
                writer.write_all(b"\n")?;
            }
        }
        Ok(())
    }
}

pub fn time_to_seconds(ts: SystemTime) -> i64 {
    match ts.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            // after epoch
            i64::try_from(d.as_secs()).unwrap()
        }
        Err(e) => {
            // before epoch
            -i64::try_from(e.duration().as_secs()).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn item(command: &str, first: u64, last: u64) -> HistoryItem {
        HistoryItem::new(
            command.into(),
            Timestamps {
                last_added: UNIX_EPOCH + Duration::from_secs(last),
                first_added: UNIX_EPOCH + Duration::from_secs(first),
            },
            PersistenceMode::Disk,
        )
    }

    #[test]
    fn shared_codec_round_trips_multiline_unicode_timestamps_and_paths() {
        let mut original = item("printf '🐟\\n'\necho done", 12, 23);
        original.set_required_paths(vec!["C:\\Users\\example".into(), "a\nb".into()]);
        let mut encoded = Vec::new();
        original.write_to(&mut encoded).unwrap();
        assert!(encoded.starts_with(b"- cmd: printf '"));
        assert!(
            encoded
                .windows(17)
                .any(|bytes| bytes == b"  added_when: 12\n")
        );
        let restored = yaml::decode_item_fish_2_0(&encoded).unwrap();
        assert_eq!(restored.str(), original.str());
        assert_eq!(restored.timestamps, original.timestamps);
        assert_eq!(restored.get_required_paths(), original.get_required_paths());
        assert!(restored.should_write_to_disk());
    }

    #[test]
    fn shared_codec_iterates_records_and_preserves_timestamp_cutoff() {
        let mut encoded = Vec::new();
        item("first", 10, 30).write_to(&mut encoded).unwrap();
        item("second", 20, 20).write_to(&mut encoded).unwrap();
        encoded.extend_from_slice(b"- cmd: incomplete");
        let mut cursor = 0;
        let mut commands = Vec::new();
        while let Some(offset) = yaml::offset_of_next_item_fish_2_0(
            &encoded,
            &mut cursor,
            Some(UNIX_EPOCH + Duration::from_secs(15)),
        ) {
            commands.push(
                yaml::decode_item_fish_2_0(&encoded[offset..])
                    .unwrap()
                    .str()
                    .to_string(),
            );
        }
        assert_eq!(commands, ["first"]);
    }

    #[test]
    fn shared_search_preserves_exact_prefix_glob_and_case_semantics() {
        let entry = item("Echo Alpha\nBeta *literal*", 0, 0);
        assert!(entry.matches_search(L!("echo alpha\nbeta *literal*"), SearchType::Exact, false));
        assert!(!entry.matches_search(L!("echo alpha\nbeta *literal*"), SearchType::Exact, true));
        assert!(entry.matches_search(L!("Echo"), SearchType::Prefix, true));
        assert!(entry.matches_search(L!("Beta"), SearchType::LinePrefix, true));
        assert!(entry.matches_search(L!("Alpha\nBeta"), SearchType::Contains, true));
        assert!(entry.matches_search(L!("echo*b*literal"), SearchType::PrefixGlob, false));
        assert!(entry.matches_search(L!("\\*literal\\*"), SearchType::ContainsGlob, true));
        assert!(entry.matches_search(L!("EhAB"), SearchType::ContainsSubsequence, true));
        assert!(!entry.matches_search(L!("echo"), SearchType::Exact, false));
    }

    #[test]
    fn merge_retains_first_creation_and_advances_last_added() {
        let mut original = item("echo same", 10, 20);
        let mut repeated = item("echo same", 30, 30);
        repeated.set_required_paths(vec!["file".into()]);
        original.merge(repeated).unwrap();
        assert_eq!(time_to_seconds(original.first_added_timestamp()), 10);
        assert_eq!(time_to_seconds(original.last_added_timestamp()), 30);
        assert_eq!(original.get_required_paths(), [WString::from("file")]);
        assert!(original.merge(item("different", 40, 40)).is_err());
        assert_eq!(time_to_seconds(UNIX_EPOCH - Duration::from_secs(5)), -5);
    }
}
