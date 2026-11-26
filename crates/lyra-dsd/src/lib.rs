//! S03 — DSD: uncompressed DSF/DFF readers and the DoP packer.
//!
//! This crate turns a DSD file (Sony `.dsf` or Philips `.dff`/DSDIFF) into a
//! [`lyra_core::Decoder`] that yields the raw 1-bit DSD byte stream **with no
//! conversion**, and packs that stream into DSD-over-PCM (DoP) 24-bit words for a
//! DoP-capable DAC. Release 1.0 handles uncompressed DSD only (ADR-008);
//! DST-compressed DFF returns [`lyra_core::LyraError::UnsupportedFormat`] and
//! SACD `.iso` is out of scope.
//!
//! # Bit order (the one thing that is easy to get wrong)
//! DSD files disagree on in-byte bit order:
//! * **DSF** stores each byte **LSB-first** ([`DsdSpec::msb_first`] = `false`).
//! * **DFF** stores each byte **MSB-first** ([`DsdSpec::msb_first`] = `true`).
//!
//! The readers deliver bytes in the file's *native* bit order and record which
//! one it is in [`Decoder::spec`]. Both readers deliver the same **byte
//! interleave**: for each byte position `i` in time, the byte of channel 0, then
//! channel 1, … For stereo that is `L0 R0 L1 R1 …`, one byte = 8 DSD samples per
//! channel.
//!
//! [`DopPacker`] re-normalises to MSB-first inside the DoP word, so a DAC receives
//! an identical DSD bitstream regardless of the container the audio came from.
//!
//! # Public API
//! ```no_run
//! use std::path::Path;
//! use lyra_dsd::{open, probe, DopPacker};
//!
//! let info = probe(Path::new("track.dff"))?;
//! println!("{} DSD samples/channel", info.total_frames);
//!
//! let mut dec = open(Path::new("track.dff"))?;
//! let mut dsd = Vec::new();
//! dec.read(&mut dsd)?;
//!
//! if let lyra_core::StreamSpec::Dsd(spec) = dec.spec() {
//!     let mut packer = DopPacker::new(spec);
//!     let mut dop = Vec::new();
//!     packer.pack(&dsd, &mut dop); // 24-bit DoP words, marker 0x05/0xFA per frame
//! }
//! # Ok::<(), lyra_core::LyraError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::Path;

use lyra_core::{Decoder, LyraError, Result};

mod decoder;
mod dff;
mod dop;
mod dsf;
mod util;

pub use decoder::DsdDecoder;
pub use dop::DopPacker;

/// Which container a DSD file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdContainer {
    /// Sony DSF (`.dsf`), stored LSB-first in per-channel blocks.
    Dsf,
    /// Philips DSDIFF (`.dff`), stored MSB-first, byte-interleaved.
    Dff,
}

/// What a recorded [`MetadataRegion`] holds.
///
/// Metadata parsing is deliberately minimal here — P6 owns full metadata parity.
/// The readers only record *where* the tag bytes sit so S06 can parse them later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataKind {
    /// An ID3v2 tag (DSF stores one at the offset named by the `DSD ` chunk).
    Id3v2,
    /// A DSDIFF `COMT` comment chunk.
    DffComment,
    /// A DSDIFF `DIIN` edited-info chunk.
    DffDiin,
}

/// The byte span of a metadata region inside the file, not yet parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataRegion {
    /// Byte offset of the region from the start of the file.
    pub offset: u64,
    /// Length of the region in bytes.
    pub len: u64,
    /// What kind of metadata the region holds.
    pub kind: MetadataKind,
}

/// A lightweight description of a DSD file: spec, exact length, container and the
/// location of any metadata, with no audio read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DsdInfo {
    /// The DSD stream format (rate, channels, bit order).
    pub spec: lyra_core::DsdSpec,
    /// Exact length in DSD samples per channel, from the header fields.
    pub total_frames: u64,
    /// Which container the file uses.
    pub container: DsdContainer,
    /// Where metadata sits, if the header points at any.
    pub metadata: Option<MetadataRegion>,
}

/// Open `path` as a DSF or DFF file and return a DSD [`Decoder`].
///
/// The container is sniffed by magic: `FRM8` → DFF, `DSD ` → DSF. The returned
/// decoder's [`Decoder::spec`] is [`lyra_core::StreamSpec::Dsd`], its
/// [`Decoder::total_frames`] is the exact DSD sample count per channel, and
/// [`Decoder::read`] appends interleaved native-bit-order DSD bytes.
///
/// # Errors
/// Returns [`LyraError::UnsupportedFormat`] for an unknown magic, a DST-compressed
/// DFF, or an unsupported DSD rate; [`LyraError::Decode`] for a malformed header;
/// [`LyraError::Io`] if the file cannot be read.
pub fn open(path: &Path) -> Result<Box<dyn Decoder>> {
    let dec = open_reader(path)?;
    Ok(Box::new(dec))
}

/// Open `path` as a concrete [`DsdDecoder`] (same sniffing as [`open`]).
///
/// # Errors
/// See [`open`].
pub fn open_reader(path: &Path) -> Result<DsdDecoder> {
    match sniff(path)? {
        DsdContainer::Dff => dff::open(path),
        DsdContainer::Dsf => dsf::open(path),
    }
}

/// Probe `path` for its spec, exact length, container and metadata location
/// without reading audio.
///
/// # Errors
/// See [`open`].
pub fn probe(path: &Path) -> Result<DsdInfo> {
    match sniff(path)? {
        DsdContainer::Dff => dff::probe(path),
        DsdContainer::Dsf => dsf::probe(path),
    }
}

/// Sniff the container of `path` by its 4-byte magic.
fn sniff(path: &Path) -> Result<DsdContainer> {
    let magic = util::read_prefix(path, 4)?;
    match &magic[..] {
        b"FRM8" => Ok(DsdContainer::Dff),
        b"DSD " => Ok(DsdContainer::Dsf),
        _ => Err(LyraError::UnsupportedFormat(format!(
            "not a DSF or DFF file (magic {magic:02x?})"
        ))),
    }
}
