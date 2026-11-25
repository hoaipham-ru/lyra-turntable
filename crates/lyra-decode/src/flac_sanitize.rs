//! Tolerant FLAC pre-processing for the metadata-block robustness gate (P1.4).
//!
//! `symphonia`'s FLAC demuxer propagates an error if any metadata block fails to
//! parse (a malformed `PICTURE`, an over-long block, …) which would abort the
//! whole file. The non-negotiable rule is that a bad *metadata* block must never
//! stop audio from decoding. When the normal open path fails on a FLAC, we rebuild
//! a minimal, well-formed metadata section here — keeping only the blocks that are
//! both essential and self-consistent (`STREAMINFO`, and any `VORBIS_COMMENT` /
//! `SEEKTABLE` that fit) and dropping everything else (pictures, padding,
//! application and unknown blocks) — then hand the reconstructed stream to
//! `symphonia`. The audio frames are preserved verbatim, so decoded PCM is
//! unchanged.
//!
//! This module also skips a leading ID3v2 tag placed in front of a FLAC stream,
//! which is otherwise non-standard for FLAC.

use lyra_core::{LyraError, Result};

const FLAC_MARKER: &[u8; 4] = b"fLaC";

// FLAC metadata block types (RFC 9639 §8.1).
const BLOCK_STREAMINFO: u8 = 0;
const BLOCK_SEEKTABLE: u8 = 3;
const BLOCK_VORBIS_COMMENT: u8 = 4;

/// Total length in bytes of a leading ID3v2 tag at the start of `data`, or `0`
/// if there is none. Handles the sync-safe size field and the optional footer.
#[must_use]
pub fn id3v2_len(data: &[u8]) -> usize {
    if data.len() < 10 || &data[0..3] != b"ID3" {
        return 0;
    }
    let flags = data[5];
    // Sync-safe 28-bit size (7 bits per byte).
    let size = (usize::from(data[6]) << 21)
        | (usize::from(data[7]) << 14)
        | (usize::from(data[8]) << 7)
        | usize::from(data[9]);
    let footer = if flags & 0x10 != 0 { 10 } else { 0 };
    10 + size + footer
}

/// Rebuild `data` (whole FLAC file bytes) into a minimal, well-formed FLAC stream
/// that `symphonia` can open, tolerating broken/oversized metadata blocks.
///
/// Returns the sanitized bytes. Errors only if there is no usable `STREAMINFO`
/// or no `fLaC` marker at all — in those cases the file is not a FLAC we can
/// salvage.
pub fn sanitize(data: &[u8]) -> Result<Vec<u8>> {
    let start = id3v2_len(data);
    let data = data
        .get(start..)
        .ok_or_else(|| LyraError::Decode("flac: truncated before stream marker".to_string()))?;

    if data.len() < 4 || &data[0..4] != FLAC_MARKER {
        return Err(LyraError::Decode("flac: missing fLaC stream marker".to_string()));
    }

    let mut pos = 4usize;
    let mut streaminfo: Option<Vec<u8>> = None;
    let mut keep: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut audio_start: Option<usize> = None;

    // Walk the metadata block headers. Each header is 4 bytes: 1 flags/type byte
    // (bit7 = last-block) + 3 bytes big-endian length.
    loop {
        if pos + 4 > data.len() {
            // Header runs past EOF: metadata section is broken. Stop; whatever we
            // gathered so far (STREAMINFO) is what we keep. No audio recoverable.
            break;
        }
        let header = data[pos];
        let is_last = header & 0x80 != 0;
        let block_type = header & 0x7F;
        let len = (usize::from(data[pos + 1]) << 16)
            | (usize::from(data[pos + 2]) << 8)
            | usize::from(data[pos + 3]);
        let body_start = pos + 4;
        let body_end = body_start.saturating_add(len);
        let fits = body_end <= data.len();

        match block_type {
            BLOCK_STREAMINFO if fits && len == 34 => {
                streaminfo = Some(data[body_start..body_end].to_vec());
            }
            BLOCK_VORBIS_COMMENT | BLOCK_SEEKTABLE if fits => {
                // Keep well-formed, essential-for-metadata/seek blocks.
                keep.push((block_type, data[body_start..body_end].to_vec()));
            }
            // PICTURE / APPLICATION / PADDING / CUESHEET / unknown / any block
            // that does not fit: drop it. This is the tolerance the gate needs.
            _ => {}
        }

        if is_last {
            audio_start = if body_end <= data.len() { Some(body_end) } else { None };
            break;
        }
        if !fits {
            // The block claims more bytes than exist: the metadata chain is
            // unrecoverable past here. We cannot locate the audio frames safely.
            break;
        }
        pos = body_end;
    }

    let streaminfo =
        streaminfo.ok_or_else(|| LyraError::Decode("flac: no usable STREAMINFO block".to_string()))?;
    let audio_start = audio_start
        .ok_or_else(|| LyraError::Decode("flac: could not locate audio after metadata".to_string()))?;
    let audio = &data[audio_start..];

    // Reassemble: marker + STREAMINFO + kept blocks + audio frames. The last
    // metadata block written gets the last-block flag.
    let mut out = Vec::with_capacity(4 + 4 + streaminfo.len() + audio.len() + 64);
    out.extend_from_slice(FLAC_MARKER);

    push_block(&mut out, BLOCK_STREAMINFO, &streaminfo, keep.is_empty());
    for (i, (block_type, body)) in keep.iter().enumerate() {
        let last = i + 1 == keep.len();
        push_block(&mut out, *block_type, body, last);
    }
    out.extend_from_slice(audio);

    Ok(out)
}

fn push_block(out: &mut Vec<u8>, block_type: u8, body: &[u8], is_last: bool) {
    let mut header = block_type & 0x7F;
    if is_last {
        header |= 0x80;
    }
    out.push(header);
    let len = body.len();
    out.push(((len >> 16) & 0xFF) as u8);
    out.push(((len >> 8) & 0xFF) as u8);
    out.push((len & 0xFF) as u8);
    out.extend_from_slice(body);
}
