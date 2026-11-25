//! Commands the UI/backend sends to the engine.

use crate::track::TrackRef;

/// A control message handed to the engine.
///
/// Mirrors the transport controls in the TypeScript `PlayerBackend`. Frame and
/// index arguments are interpreted by the engine (S05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCommand {
    /// Start or resume playback.
    Play,
    /// Pause, keeping position.
    Pause,
    /// Stop and reset position.
    Stop,
    /// Seek to a source frame.
    Seek(u64),
    /// Advance to the next queued track.
    Next,
    /// Go back to the previous queued track.
    Prev,
    /// Play the queue entry at this zero-based index.
    PlayAt(usize),
    /// Replace the play queue.
    SetQueue(Vec<TrackRef>),
}
