//! Track identity and the resolved track description.

use crate::spec::StreamSpec;

/// A reference to a playable track: a source path and an optional sub-track.
///
/// `path` is the file location as a plain string (the engine, not this crate,
/// resolves it on the filesystem). `subtrack` indexes an entry inside a cue
/// sheet or multi-track container; `None` means the whole file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackRef {
    /// Source file path.
    pub path: String,
    /// Zero-based sub-track index for cue sheets, or `None` for the whole file.
    pub subtrack: Option<u32>,
}

impl TrackRef {
    /// Create a reference to a whole file.
    #[must_use]
    pub fn file(path: impl Into<String>) -> Self {
        Self { path: path.into(), subtrack: None }
    }
}

/// Resolved information about the track currently being played.
///
/// Emitted by the engine once a track is opened and its format known. Metadata
/// fields are optional because not every source carries them.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    /// The reference this info describes.
    pub track: TrackRef,
    /// Track title, if tagged.
    pub title: Option<String>,
    /// Primary artist, if tagged.
    pub artist: Option<String>,
    /// Album name, if tagged.
    pub album: Option<String>,
    /// Exact length in source frames, if known.
    pub duration_frames: Option<u64>,
    /// The decoded stream format.
    pub spec: StreamSpec,
}
