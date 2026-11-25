//! S02 — decode: native-PCM decoding of FLAC and WAV (symphonia backend).
//!
//! This crate turns an encoded file into a [`lyra_core::Decoder`] that yields
//! interleaved PCM frames with **no conversion**: the source sample rate and
//! channel count are preserved, and each sample is packed into the smallest
//! container that losslessly holds its valid bits (ADR-004). There is no
//! resampling, dither, float conversion or volume on this path.
//!
//! # Scope (P1)
//! * FLAC via `symphonia` (all block sizes, 16/24-bit, 44.1–192 kHz).
//! * WAV / `WAVE_FORMAT_EXTENSIBLE` / RF64 via `symphonia`.
//! * Exact [`Decoder::total_frames`] from STREAMINFO / the WAV data chunk, equal to
//!   foobar2000 `%length_samples%`.
//! * Sample-accurate [`Decoder::seek`]: the next `read` starts exactly at the
//!   requested frame (decode-from-sync-point then discard).
//! * Metadata-block tolerance: a broken/oversized `PICTURE` or padding block, or a
//!   leading ID3 tag, never stops audio from decoding. Corrupt *audio* returns a
//!   [`lyra_core::LyraError::Decode`] whose message carries the frame position; the
//!   decoder never panics on malformed input.
//!
//! # Public API
//! ```no_run
//! use std::path::Path;
//! use lyra_decode::{open, probe, OpenOptions};
//!
//! let info = probe(Path::new("track.flac"))?;
//! println!("{} frames, codec {}", info.total_frames.unwrap_or(0), info.codec);
//!
//! let mut dec = open(Path::new("track.flac"), &OpenOptions::default())?;
//! let mut buf = Vec::new();
//! let frames = dec.read(&mut buf)?;
//! # Ok::<(), lyra_core::LyraError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::Path;

use lyra_core::{Decoder, Result, StreamSpec, TagMap};

mod backend;
mod flac_sanitize;
mod target;

#[cfg(feature = "libflac-check")]
pub mod libflac;

pub use target::Container24;

/// Options controlling how a file is opened.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenOptions {
    /// How a 24-bit source is carried in the output container (ADR-004).
    ///
    /// Defaults to [`Container24::Packed`] (24-in-3 bytes), which is byte-identical
    /// to the `flac -d` / `ffmpeg pcm_s24le` golden references.
    pub container_24bit: Container24,
}

impl OpenOptions {
    /// Default options (24-bit packed in 3 bytes).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Carry 24-bit sources left-justified in a 32-bit word (ADR-004 widening).
    #[must_use]
    pub fn with_24bit_in_s32(mut self) -> Self {
        self.container_24bit = Container24::LeftJustifiedS32;
        self
    }
}

/// Lightweight description of a file: spec, exact length, codec and tags, with no
/// full audio decode.
#[derive(Debug, Clone)]
pub struct ProbeInfo {
    /// The native stream format the decoder would produce.
    pub spec: StreamSpec,
    /// Exact length in source frames (`None` if the container does not state it).
    pub total_frames: Option<u64>,
    /// Codec name: `"flac"` or `"wav"`.
    pub codec: &'static str,
    /// Metadata tags.
    pub tags: TagMap,
}

/// Open `path` and return a native-PCM [`Decoder`].
///
/// Sniffs FLAC vs WAV by extension and content. For FLAC, a first failure (e.g. a
/// broken metadata block or a leading ID3 tag) triggers a tolerant rebuild of the
/// metadata section before retrying, so a bad metadata block never stops playback.
///
/// # Errors
/// Returns [`lyra_core::LyraError`] for an unsupported container, an unreadable
/// file, or a stream with no decodable audio track.
pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Decoder>> {
    let dec = backend::open(path, opts)?;
    Ok(Box::new(dec))
}

/// Probe `path` for its spec, exact length, codec and tags without decoding audio.
///
/// # Errors
/// Same conditions as [`open`].
pub fn probe(path: &Path) -> Result<ProbeInfo> {
    let dec = backend::open(path, &OpenOptions::default())?;
    Ok(dec.probe_info())
}
