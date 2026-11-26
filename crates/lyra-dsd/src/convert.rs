//! DFF → DSF transcoding (P5.3): re-container uncompressed DSD, unchanged audio.
//!
//! The logical DSD bitstream is preserved exactly; only the container changes.
//! DFF stores bytes **MSB-first** and channel-interleaved, DSF stores them
//! **LSB-first** in per-channel blocks, so each byte is bit-reversed and the
//! stream is de-interleaved into `block size` (4096-byte) blocks, the final block
//! of each channel zero-padded. This is the inverse normalisation the
//! [`crate::DopPacker`] relies on, verified by the round-trip test (decode both
//! containers → identical DSD after bit-order normalisation).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use lyra_core::{Decoder, LyraError, Result, StreamSpec};

use crate::open;

/// DSF block size per channel, in bytes (the conventional 4096).
const BLOCK: u64 = 4096;

/// Convert the uncompressed DSD file at `input` (DFF) to a DSF file at `output`.
///
/// `output` is created/overwritten. Only uncompressed sources are supported; a
/// DST-compressed DFF is rejected by [`open`] before this runs.
///
/// # Errors
/// [`LyraError`] for an unreadable/unsupported source or any write failure.
pub fn dff_to_dsf(input: &Path, output: &Path) -> Result<()> {
    let dec = open(input)?;
    let out = File::create(output).map_err(|e| LyraError::Io(e.to_string()))?;
    let mut w = BufWriter::new(out);
    write_dsf(dec, &mut w)?;
    w.flush().map_err(|e| LyraError::Io(e.to_string()))?;
    Ok(())
}

/// Write `dec`'s DSD stream to `w` as a DSF file.
///
/// Works for any uncompressed DSD decoder; if the source is already LSB-first the
/// bytes are copied as-is, otherwise each byte is bit-reversed.
pub fn write_dsf<W: Write>(mut dec: Box<dyn Decoder>, w: &mut W) -> Result<()> {
    let spec = match dec.spec() {
        StreamSpec::Dsd(s) => s,
        StreamSpec::Pcm(_) => {
            return Err(LyraError::UnsupportedFormat(
                "cannot write PCM as DSF".to_string(),
            ))
        }
    };
    let channels = spec.channels as usize;
    if channels == 0 {
        return Err(LyraError::Decode("source has zero channels".to_string()));
    }
    // DSF is LSB-first: flip only if the source is MSB-first (DFF).
    let flip = spec.msb_first;

    let total_frames = dec
        .total_frames()
        .ok_or_else(|| LyraError::Decode("source has unknown length".to_string()))?;
    let bytes_per_channel = total_frames.div_ceil(8);
    let groups = bytes_per_channel.div_ceil(BLOCK);
    let data_size = groups * BLOCK * channels as u64;

    write_header(w, &spec, total_frames, data_size)?;

    // Stream the interleaved DSD, flushing one DSF block-group (BLOCK bytes per
    // channel) at a time. Each decoder read yields a whole number of bytes per
    // channel, so `pending` always holds a balanced interleave.
    let group_bytes = (BLOCK as usize) * channels;
    let mut pending: Vec<u8> = Vec::with_capacity(group_bytes * 2);
    loop {
        let before = pending.len();
        let n = dec.read(&mut pending)?;
        if n == 0 {
            if pending.len() != before {
                return Err(LyraError::Decode("decoder returned 0 with bytes".to_string()));
            }
            break;
        }
        while pending.len() >= group_bytes {
            write_group(w, &pending[..group_bytes], channels, BLOCK as usize, flip)?;
            pending.drain(0..group_bytes);
        }
    }
    // Final partial group: pad each channel's block to BLOCK with zeros.
    if !pending.is_empty() {
        write_group(w, &pending, channels, BLOCK as usize, flip)?;
    }
    Ok(())
}

/// Write one DSF block-group from `interleaved` bytes (`per_channel = len/channels`,
/// padded up to `block`), de-interleaving, optionally bit-reversing.
fn write_group<W: Write>(
    w: &mut W,
    interleaved: &[u8],
    channels: usize,
    block: usize,
    flip: bool,
) -> Result<()> {
    let per_channel = interleaved.len() / channels;
    let mut blk = vec![0u8; block];
    for ch in 0..channels {
        blk.iter_mut().for_each(|b| *b = 0);
        for j in 0..per_channel {
            let byte = interleaved[j * channels + ch];
            blk[j] = if flip { byte.reverse_bits() } else { byte };
        }
        w.write_all(&blk).map_err(|e| LyraError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Write the 92-byte DSF header (`DSD `, `fmt `, `data` chunk start).
fn write_header<W: Write>(
    w: &mut W,
    spec: &lyra_core::DsdSpec,
    total_frames: u64,
    data_size: u64,
) -> Result<()> {
    let channels = u32::from(spec.channels);
    let data_chunk_size = data_size + 12;
    let total_file_size = 92 + data_size;

    let mut h: Vec<u8> = Vec::with_capacity(92);
    // DSD chunk (28 bytes).
    h.extend_from_slice(b"DSD ");
    h.extend_from_slice(&28u64.to_le_bytes());
    h.extend_from_slice(&total_file_size.to_le_bytes());
    h.extend_from_slice(&0u64.to_le_bytes()); // no metadata pointer
    // fmt chunk (52 bytes total: 12 header + 40 body).
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&52u64.to_le_bytes());
    h.extend_from_slice(&1u32.to_le_bytes()); // format version
    h.extend_from_slice(&0u32.to_le_bytes()); // format id (DSD raw)
    h.extend_from_slice(&channels.to_le_bytes()); // channel type (1=mono,2=stereo,…)
    h.extend_from_slice(&channels.to_le_bytes()); // channel num
    h.extend_from_slice(&spec.rate.hz().to_le_bytes()); // sampling frequency
    h.extend_from_slice(&1u32.to_le_bytes()); // bits per sample
    h.extend_from_slice(&total_frames.to_le_bytes()); // sample count (per channel)
    h.extend_from_slice(&(BLOCK as u32).to_le_bytes()); // block size per channel
    h.extend_from_slice(&0u32.to_le_bytes()); // reserved
    // data chunk header (12 bytes).
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_chunk_size.to_le_bytes());

    w.write_all(&h).map_err(|e| LyraError::Io(e.to_string()))?;
    Ok(())
}
