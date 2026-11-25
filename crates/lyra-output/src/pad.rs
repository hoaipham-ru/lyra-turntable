//! Zero-LSB container padding (ADR-004).
//!
//! When the negotiated container is wider than the source precision, source
//! samples are placed MSB-aligned with zero-padded low bits. This is a lossless
//! transform (shifting left by whole bytes) and keeps the path bit-perfect; QA's
//! `lyra-probe compare` shifts the tapped bytes back before comparing to golden.
//!
//! This runs on the normal (producer) thread, never on the real-time render
//! thread — the render thread only copies already-padded bytes into the device
//! buffer.

use lyra_core::{PcmSpec, SampleFormat};

/// Pad `input` (interleaved samples in `src.format`) into the `target` container,
/// appending the result to `out` (which is cleared first).
///
/// Because samples are little-endian and each sample is independent, this simply
/// prepends `target_bytes - src_bytes` zero bytes before each source sample — the
/// source sits in the high bytes, the padding in the low (least-significant)
/// bytes. `target` must be at least as wide as `src.format`; negotiation
/// guarantees this, so a narrower target is treated as a programming error.
pub fn pad_frames(src: PcmSpec, target: SampleFormat, input: &[u8], out: &mut Vec<u8>) {
    let sbytes = src.format.bytes_per_sample();
    let tbytes = target.bytes_per_sample();
    out.clear();

    debug_assert!(
        tbytes >= sbytes,
        "pad target {tbytes}B narrower than source {sbytes}B would drop bits",
    );

    if tbytes == sbytes {
        out.extend_from_slice(input);
        return;
    }

    let pad = tbytes - sbytes;
    out.reserve(input.len() / sbytes.max(1) * tbytes);
    for sample in input.chunks_exact(sbytes) {
        for _ in 0..pad {
            out.push(0);
        }
        out.extend_from_slice(sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(format: SampleFormat, bits_valid: u8) -> PcmSpec {
        PcmSpec { rate: 48_000, channels: 2, bits_valid, format }
    }

    #[test]
    fn identity_when_same_width() {
        let input = [0x11, 0x22, 0x33, 0x44];
        let mut out = Vec::new();
        pad_frames(spec(SampleFormat::S16, 16), SampleFormat::S16, &input, &mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn s16_into_s32_msb_aligned() {
        // One sample, LE value 0x2211. Expected 32-bit LE: 0x00 0x00 0x11 0x22.
        let input = [0x11, 0x22];
        let mut out = Vec::new();
        pad_frames(spec(SampleFormat::S16, 16), SampleFormat::S32, &input, &mut out);
        assert_eq!(out, [0x00, 0x00, 0x11, 0x22]);
    }

    #[test]
    fn s24_into_s32_low_byte_zero() {
        // Packed 24-bit LE sample 0x332211 -> 0x00 0x11 0x22 0x33.
        let input = [0x11, 0x22, 0x33];
        let mut out = Vec::new();
        pad_frames(spec(SampleFormat::S24, 24), SampleFormat::S32, &input, &mut out);
        assert_eq!(out, [0x00, 0x11, 0x22, 0x33]);
    }

    #[test]
    fn s16_into_s24_low_byte_zero() {
        let input = [0x11, 0x22];
        let mut out = Vec::new();
        pad_frames(spec(SampleFormat::S16, 16), SampleFormat::S24, &input, &mut out);
        assert_eq!(out, [0x00, 0x11, 0x22]);
    }

    #[test]
    fn multi_sample_stereo_s16_to_s32() {
        // Two samples (L, R).
        let input = [0x01, 0x02, 0x03, 0x04];
        let mut out = Vec::new();
        pad_frames(spec(SampleFormat::S16, 16), SampleFormat::S32, &input, &mut out);
        assert_eq!(out, [0x00, 0x00, 0x01, 0x02, 0x00, 0x00, 0x03, 0x04]);
    }
}
