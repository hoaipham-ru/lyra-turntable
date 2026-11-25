//! Events the engine emits to the UI/backend.

use crate::error::LyraError;
use crate::spec::StreamSpec;
use crate::track::TrackInfo;

/// Transport play state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlayState {
    /// Nothing is loaded or playback is stopped at the start.
    Stopped,
    /// Audio is advancing.
    Playing,
    /// Loaded and positioned, but not advancing.
    Paused,
}

/// An asynchronous notification from the engine.
///
/// Delivered over a channel to the backend; it is `Clone` + `PartialEq` so it can
/// be fanned out and asserted on in tests. It never borrows audio data, so it has
/// no lifetime.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// Transport state changed.
    State(PlayState),
    /// Playback position advanced to `frame` (source frames from the start).
    Position {
        /// Current position in source frames.
        frame: u64,
    },
    /// A new track started; carries its resolved info.
    Track(TrackInfo),
    /// The source/device formats were negotiated.
    Format {
        /// The decoded source format.
        source: StreamSpec,
        /// The format handed to the device.
        device: StreamSpec,
        /// `true` when the device stream equals the source bit-for-bit
        /// (ADR-004); `false` once any processing is applied.
        bit_perfect: bool,
    },
    /// The output ring buffer ran dry; `count` is the running total.
    Underrun {
        /// Cumulative underrun count since playback started.
        count: u64,
    },
    /// A recoverable or fatal error occurred.
    Error(LyraError),
}
