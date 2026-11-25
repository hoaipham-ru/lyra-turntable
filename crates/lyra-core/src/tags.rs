//! Metadata tag storage.

use std::collections::BTreeMap;

/// A case-sensitive map of metadata tag keys to values.
///
/// Keys follow the source container's own naming (e.g. Vorbis comment field
/// names like `TITLE`, `ARTIST`, `ALBUM`). The map keeps insertion-independent,
/// sorted iteration order via [`BTreeMap`] so output is deterministic across runs
/// (helpful for parity reports). Multi-valued tags are not modelled here in v0.1;
/// a later contract bump may add them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagMap {
    entries: BTreeMap<String, String>,
}

impl TagMap {
    /// Create an empty tag map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a tag, returning the previous value if any.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) -> Option<String> {
        self.entries.insert(key.into(), value.into())
    }

    /// Look up a tag by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// Number of tags stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if no tags are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over `(key, value)` pairs in sorted key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let mut tags = TagMap::new();
        assert!(tags.is_empty());
        assert_eq!(tags.insert("TITLE", "Song"), None);
        assert_eq!(tags.insert("TITLE", "New"), Some("Song".to_string()));
        assert_eq!(tags.get("TITLE"), Some("New"));
        assert_eq!(tags.get("MISSING"), None);
        assert_eq!(tags.len(), 1);
    }

    #[test]
    fn iterates_sorted() {
        let mut tags = TagMap::new();
        tags.insert("ZEBRA", "z");
        tags.insert("ALBUM", "a");
        let keys: Vec<&str> = tags.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["ALBUM", "ZEBRA"]);
    }
}
