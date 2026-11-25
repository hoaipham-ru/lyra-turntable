//! The decoder trait implemented by every format reader (S02, S03).

use crate::error::Result;
use crate::spec::StreamSpec;
use crate::tags::TagMap;

/// A streaming source decoder.
///
/// Implementations read an encoded file and produce interleaved samples in the
/// format reported by [`Decoder::spec`]. The trait is `Send` so a decoder can be
/// moved onto the engine's decode thread; it is intentionally *not* `Sync`, as a
/// decoder holds mutable stream position.
pub trait Decoder: Send {
    /// The format of the frames this decoder produces.
    fn spec(&self) -> StreamSpec;

    /// Exact stream length in source frames, or `None` if unknown.
    fn total_frames(&self) -> Option<u64>;

    /// Seek to `frame` and return the actual frame landed on.
    ///
    /// Implementations may land on the nearest decodable boundary at or before
    /// the request; the returned value is authoritative.
    fn seek(&mut self, frame: u64) -> Result<u64>;

    /// Decode the next block, appending interleaved bytes to `out`.
    ///
    /// Returns the number of frames appended. A return of `0` means end of
    /// stream. `out` is appended to, never cleared, so the caller controls
    /// buffering and allocation.
    fn read(&mut self, out: &mut Vec<u8>) -> Result<usize>;

    /// Metadata tags read from the source.
    fn tags(&self) -> &TagMap;
}
