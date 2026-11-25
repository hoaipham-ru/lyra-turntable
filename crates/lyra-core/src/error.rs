//! The shared error type and `Result` alias.

use thiserror::Error;

/// Errors surfaced across Lyra crates.
///
/// Variants carry a human-readable message rather than a wrapped source error so
/// the type stays `Clone` and `PartialEq` (it travels inside [`crate::EngineEvent`]
/// across thread and channel boundaries). Crates performing real I/O convert their
/// own errors into the appropriate variant at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LyraError {
    /// The container or codec is not supported.
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    /// Decoding failed (corrupt stream, codec error).
    #[error("decode error: {0}")]
    Decode(String),

    /// An underlying I/O operation failed, described by the message.
    #[error("i/o error: {0}")]
    Io(String),

    /// The output device rejected an operation or format.
    #[error("device error: {0}")]
    Device(String),

    /// A seek targeted a frame outside the stream.
    #[error("seek out of range: frame {0}")]
    SeekOutOfRange(u64),

    /// The stream ended.
    #[error("end of stream")]
    EndOfStream,

    /// A command was invalid in the current state (described by the message).
    #[error("invalid state: {0}")]
    InvalidState(String),
}

/// Convenience alias for results that fail with [`LyraError`].
pub type Result<T> = core::result::Result<T, LyraError>;
