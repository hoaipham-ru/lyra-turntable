//! Lyra's own decoder path (P1.5): decode a file to raw interleaved PCM with the
//! merged `lyra-decode` crate, so parity is tested against the *real* engine
//! decoder rather than the P0 ffmpeg reference.
//!
//! The bytes produced here use `OpenOptions::default()` — 24-bit sources packed in
//! 3 bytes (`Container24::Packed`) — which is the byte layout of the libFLAC /
//! `ffmpeg pcm_s24le` golden references, so a Lyra decode can be hashed and
//! compared directly against them (TEST-STRATEGY §2, ADR-004).

use anyhow::{bail, Context, Result};
use lyra_core::StreamSpec;
use lyra_decode::{open, OpenOptions};
use std::path::Path;

use crate::ffmpeg::Decoded;

/// Decode `input` to raw interleaved PCM with `lyra-decode`.
///
/// Returns the native [`lyra_core::PcmSpec`] the engine chose plus the raw
/// little-endian interleaved payload (no container header), the same shape as the
/// ffmpeg reference path so callers can treat the two interchangeably.
pub fn decode_raw(input: &Path) -> Result<Decoded> {
    let mut dec = open(input, &OpenOptions::default())
        .with_context(|| format!("lyra-decode failed to open {}", input.display()))?;

    let spec = match dec.spec() {
        StreamSpec::Pcm(p) => p,
        StreamSpec::Dsd(_) => bail!(
            "lyra-decode produced a DSD stream for {}; `probe decode` handles PCM only (DSD is P5)",
            input.display()
        ),
    };

    // Pre-size the buffer from the exact length when the container states it, so a
    // whole-file decode does not repeatedly reallocate.
    let mut pcm = match dec.total_frames() {
        Some(frames) => Vec::with_capacity(frames as usize * spec.bytes_per_frame()),
        None => Vec::new(),
    };

    loop {
        let n = dec
            .read(&mut pcm)
            .with_context(|| format!("lyra-decode read error in {}", input.display()))?;
        if n == 0 {
            break;
        }
    }

    Ok(Decoded { spec, pcm })
}

/// Seek to `seek_frame` (if given) and decode at most `max_frames` frames (if
/// given), for seek-parity against a golden slice (TEST-STRATEGY §4).
///
/// The returned payload starts exactly at `seek_frame` and is truncated to
/// `max_frames` frames, so it compares directly against
/// `golden[seek_frame .. seek_frame + max_frames]`.
pub fn decode_slice(
    input: &Path,
    seek_frame: Option<u64>,
    max_frames: Option<u64>,
) -> Result<Decoded> {
    let mut dec = open(input, &OpenOptions::default())
        .with_context(|| format!("lyra-decode failed to open {}", input.display()))?;

    let spec = match dec.spec() {
        StreamSpec::Pcm(p) => p,
        StreamSpec::Dsd(_) => bail!(
            "lyra-decode produced a DSD stream for {}; `probe decode` handles PCM only (DSD is P5)",
            input.display()
        ),
    };
    let bpf = spec.bytes_per_frame();

    if let Some(frame) = seek_frame {
        let landed = dec
            .seek(frame)
            .with_context(|| format!("lyra-decode seek to frame {frame} in {}", input.display()))?;
        // The engine's seek contract is sample-accurate: the next read starts at
        // exactly the requested frame. Surface a violation loudly.
        if landed != frame {
            bail!("seek landed on frame {landed}, expected {frame} (not sample-accurate)");
        }
    }

    let want_bytes = max_frames.map(|f| f as usize * bpf);
    let mut pcm = match want_bytes {
        Some(b) => Vec::with_capacity(b),
        None => Vec::new(),
    };

    loop {
        let n = dec
            .read(&mut pcm)
            .with_context(|| format!("lyra-decode read error in {}", input.display()))?;
        if n == 0 {
            break;
        }
        if let Some(limit) = want_bytes {
            if pcm.len() >= limit {
                pcm.truncate(limit);
                break;
            }
        }
    }

    Ok(Decoded { spec, pcm })
}
