//! The DoP (DSD-over-PCM) packer.
//!
//! DoP carries DSD inside a normal 24-bit PCM stream so a DoP-capable DAC can
//! recover the 1-bit samples. Each 24-bit PCM sample holds, from most- to
//! least-significant byte:
//!
//! ```text
//!   bits 23..16 : marker byte, 0x05 then 0xFA, alternating every PCM frame
//!   bits 15..8  : first (older) DSD byte of this channel, MSB-first
//!   bits  7..0  : second (newer) DSD byte of this channel, MSB-first
//! ```
//!
//! One PCM frame carries two consecutive DSD bytes per channel, so the carrier
//! PCM rate is the DSD rate / 16 ([`lyra_core::DsdRate::dop_pcm_rate`]). The input
//! is the common channel-interleaved DSD byte stream from the readers; a frame's
//! `2 * channels` input bytes are `[A_ch0, A_ch1, …, B_ch0, B_ch1, …]`, where
//! `A` is the older byte.
//!
//! The DSD bits inside the word are always MSB-first regardless of the source
//! container: when the source is LSB-first (DSF) each byte is bit-reversed first,
//! so the DAC receives an identical bitstream whether the audio came from DSF or
//! DFF. The packer **never scales** the words (ADR-004: DoP is bit-exact DSD).
//!
//! The marker alternation is held in packer state, so it stays continuous across
//! `pack` calls of any size (buffer boundaries, pause/resume, gapless joins).
//! Partial frames at the end of a call are buffered until the next call.
//! [`DopPacker::reset`] clears that buffer and restarts the marker at `0x05` — use
//! it only at a true track start, not across a gapless join.

use lyra_core::{DsdRate, DsdSpec, PcmSpec, SampleFormat};

/// Packs interleaved DSD bytes into DoP 24-bit PCM words.
pub struct DopPacker {
    channels: usize,
    msb_first: bool,
    rate: DsdRate,
    /// `true` when the next PCM frame uses marker `0x05`, `false` for `0xFA`.
    marker_high: bool,
    /// Leftover input bytes (`< 2 * channels`) awaiting a full PCM frame.
    pending: Vec<u8>,
}

/// The DoP start marker (first frame, bits 23..16).
const MARKER_A: u8 = 0x05;
/// The DoP alternate marker (second frame).
const MARKER_B: u8 = 0xFA;

impl DopPacker {
    /// Create a packer for a DSD stream.
    ///
    /// `spec` supplies the channel count and the source bit order (`msb_first`);
    /// the carrier PCM rate comes from `spec.rate`.
    #[must_use]
    pub fn new(spec: DsdSpec) -> Self {
        DopPacker {
            channels: spec.channels as usize,
            msb_first: spec.msb_first,
            rate: spec.rate,
            marker_high: true,
            pending: Vec::new(),
        }
    }

    /// The PCM carrier format this packer produces: 24-bit, `rate / 16` Hz.
    #[must_use]
    pub fn out_spec(&self) -> PcmSpec {
        PcmSpec {
            rate: self.rate.dop_pcm_rate(),
            channels: self.channels as u16,
            bits_valid: 24,
            format: SampleFormat::S24,
        }
    }

    /// Reset marker and drop any buffered partial frame (use at a track start).
    pub fn reset(&mut self) {
        self.marker_high = true;
        self.pending.clear();
    }

    /// Pack `dsd` into `out`, appending little-endian 24-bit DoP words.
    ///
    /// `out` is appended to, never cleared. Any trailing bytes that do not make a
    /// whole PCM frame are held for the next call, so marker alternation is
    /// continuous no matter how the input is chunked.
    pub fn pack(&mut self, dsd: &[u8], out: &mut Vec<u8>) {
        let frame_bytes = self.channels * 2; // input bytes per PCM frame
        if frame_bytes == 0 {
            return;
        }

        // Combine any buffered partial frame with the new input, then process all
        // whole frames. `pending` is always shorter than one frame.
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(dsd);

        let full_frames = buf.len() / frame_bytes;
        out.reserve(full_frames * self.channels * 3);

        for f in 0..full_frames {
            let base = f * frame_bytes;
            let marker = if self.marker_high { MARKER_A } else { MARKER_B };
            for ch in 0..self.channels {
                let a = buf[base + ch];
                let b = buf[base + self.channels + ch];
                let (a, b) = if self.msb_first {
                    (a, b)
                } else {
                    (a.reverse_bits(), b.reverse_bits())
                };
                // 24-bit value (marker<<16 | a<<8 | b) in little-endian bytes.
                out.push(b);
                out.push(a);
                out.push(marker);
            }
            self.marker_high = !self.marker_high;
        }

        let consumed = full_frames * frame_bytes;
        buf.drain(0..consumed);
        self.pending = buf;
    }
}
