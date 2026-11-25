//! Exclusive-mode format negotiation (P2.1).
//!
//! This module is pure, platform-independent logic so it can be unit-tested
//! against a mocked capability set without ever touching a real device
//! (S04 "Mock … for tests"). The WASAPI backend ([`crate::WasapiDevice`]) feeds
//! it the results of `IsFormatSupported` probes; the chosen container is then
//! what the render thread hands to the device.
//!
//! The one rule that matters: **never lose bits and never resample**. A format is
//! only acceptable if its container physically holds every meaningful source bit
//! at the exact source sample rate; extra low bits are zero-padded (ADR-004) and
//! the result is flagged [`Negotiated::padded`].

use lyra_core::{LyraError, PcmSpec, Result, SampleFormat};

/// A concrete interleaved PCM format a device can be asked for in exclusive mode:
/// a sample rate, a container word ([`SampleFormat`]) and the number of valid
/// bits the driver is told to expect (`wValidBitsPerSample`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExclusiveFormat {
    /// Sample rate in Hz. Negotiation never crosses rates (no resampling).
    pub rate: u32,
    /// Container word size carried to the device.
    pub format: SampleFormat,
    /// Valid bits advertised to the driver for this container.
    pub valid_bits: u8,
}

/// The container / valid-bit pairs Lyra probes at each source rate (S04 table):
/// packed 24-in-3, 24-in-32, full 32 and 16. Float is never probed — the
/// bit-perfect path is integer only (ADR-004); a float source must be decoded to
/// an integer container upstream.
pub const PROBE_CANDIDATES: [(SampleFormat, u8); 4] = [
    (SampleFormat::S24, 24), // 24-in-3 (packed)
    (SampleFormat::S32, 24), // 24-in-32
    (SampleFormat::S32, 32), // S32
    (SampleFormat::S16, 16), // S16
];

/// The outcome of negotiating one source [`PcmSpec`] against a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Negotiated {
    /// What the device will actually be driven with. `bits_valid` stays the
    /// *source* precision; `format` is the chosen container.
    pub spec: PcmSpec,
    /// Value written to `wValidBitsPerSample` when opening the stream. This is
    /// the probed valid-bit count the driver accepted, not the source precision.
    pub request_valid_bits: u8,
    /// `true` when the container is wider than the source precision, so the
    /// source bits are MSB-aligned with zero-padded LSBs (must be reported,
    /// ADR-004). Still bit-perfect.
    pub padded: bool,
}

/// Number of bits the container physically holds.
const fn container_bits(format: SampleFormat) -> u32 {
    (format.bytes_per_sample() * 8) as u32
}

/// Pick the exclusive-mode container that carries `src` losslessly at its own
/// rate, preferring the narrowest exact fit.
///
/// `supported` is the set of formats the device accepted in exclusive mode
/// (typically the result of probing [`PROBE_CANDIDATES`] at `src.rate`).
///
/// # Errors
/// - [`LyraError::UnsupportedFormat`] if `src` is float (no integer container).
/// - [`LyraError::Device`] if no supported container can hold the source rate and
///   precision without resampling or dropping bits.
pub fn negotiate_exclusive(src: PcmSpec, supported: &[ExclusiveFormat]) -> Result<Negotiated> {
    if matches!(src.format, SampleFormat::F32) {
        return Err(LyraError::UnsupportedFormat(
            "float PCM has no bit-perfect integer container; decode to integer first".into(),
        ));
    }

    let mut best: Option<(u32, ExclusiveFormat)> = None;
    for cand in supported {
        if cand.rate != src.rate {
            continue; // never resample
        }
        if container_bits(cand.format) < u32::from(src.bits_valid) {
            continue; // container too small: would truncate meaningful bits
        }
        if u32::from(cand.valid_bits) < u32::from(src.bits_valid) {
            // Advertising fewer valid bits than the source has would tell the DAC
            // to treat the low source bits as padding and drop them — not
            // bit-perfect. Only accept containers whose advertised valid-bit count
            // covers the whole source precision.
            continue;
        }
        // Lower rank is better: narrowest container first, then fewest advertised
        // valid bits. This yields S16 → S24 → 24-in-32 → S32 for a 16-bit source,
        // and S24 → 24-in-32 → S32 for a 24-bit source.
        let rank = container_bits(cand.format) * 100 + u32::from(cand.valid_bits);
        if best.is_none_or(|(r, _)| rank < r) {
            best = Some((rank, *cand));
        }
    }

    let (_, chosen) = best.ok_or_else(|| {
        LyraError::Device(format!(
            "device accepts no exclusive container for {}-bit audio at {} Hz without resampling",
            src.bits_valid, src.rate
        ))
    })?;

    let padded = container_bits(chosen.format) != u32::from(src.bits_valid);
    Ok(Negotiated {
        spec: PcmSpec {
            rate: src.rate,
            channels: src.channels,
            bits_valid: src.bits_valid,
            format: chosen.format,
        },
        request_valid_bits: chosen.valid_bits,
        padded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(rate: u32, format: SampleFormat, valid_bits: u8) -> ExclusiveFormat {
        ExclusiveFormat { rate, format, valid_bits }
    }

    fn src(rate: u32, bits_valid: u8, format: SampleFormat) -> PcmSpec {
        PcmSpec { rate, channels: 2, bits_valid, format }
    }

    #[test]
    fn exact_16_bit_prefers_s16() {
        let supported = [
            fmt(44_100, SampleFormat::S16, 16),
            fmt(44_100, SampleFormat::S32, 32),
        ];
        let n = negotiate_exclusive(src(44_100, 16, SampleFormat::S16), &supported).unwrap();
        assert_eq!(n.spec.format, SampleFormat::S16);
        assert!(!n.padded);
        assert_eq!(n.request_valid_bits, 16);
        assert_eq!(n.spec.bits_valid, 16);
    }

    #[test]
    fn sixteen_bit_pads_up_when_no_s16() {
        // Device (like the dev laptop codec) only offers 24-in-32 and S32.
        let supported = [
            fmt(48_000, SampleFormat::S32, 24),
            fmt(48_000, SampleFormat::S32, 32),
        ];
        let n = negotiate_exclusive(src(48_000, 16, SampleFormat::S16), &supported).unwrap();
        assert_eq!(n.spec.format, SampleFormat::S32);
        assert!(n.padded);
        // 24-in-32 ranks below full S32, so it is chosen.
        assert_eq!(n.request_valid_bits, 24);
        assert_eq!(n.spec.bits_valid, 16); // meaningful bits preserved
    }

    #[test]
    fn twenty_four_bit_prefers_packed() {
        let supported = [
            fmt(96_000, SampleFormat::S24, 24),
            fmt(96_000, SampleFormat::S32, 24),
            fmt(96_000, SampleFormat::S32, 32),
        ];
        let n = negotiate_exclusive(src(96_000, 24, SampleFormat::S24), &supported).unwrap();
        assert_eq!(n.spec.format, SampleFormat::S24);
        assert!(!n.padded);
    }

    #[test]
    fn twenty_four_bit_uses_24_in_32_when_packed_absent() {
        let supported = [
            fmt(96_000, SampleFormat::S32, 24),
            fmt(96_000, SampleFormat::S32, 32),
        ];
        let n = negotiate_exclusive(src(96_000, 24, SampleFormat::S24), &supported).unwrap();
        assert_eq!(n.spec.format, SampleFormat::S32);
        assert_eq!(n.request_valid_bits, 24);
        assert!(n.padded); // 24 bits in a 32-bit container is padded (ADR-004)
    }

    #[test]
    fn never_chooses_a_narrower_container() {
        // Only S16 offered, but the source is 24-bit: must fail, not truncate.
        let supported = [fmt(96_000, SampleFormat::S16, 16)];
        let err = negotiate_exclusive(src(96_000, 24, SampleFormat::S24), &supported).unwrap_err();
        assert!(matches!(err, LyraError::Device(_)));
    }

    #[test]
    fn rejects_rate_mismatch_no_resample() {
        // Device supports 48 kHz only; a 44.1 kHz source must not be remapped.
        let supported = [
            fmt(48_000, SampleFormat::S32, 24),
            fmt(48_000, SampleFormat::S32, 32),
        ];
        let err = negotiate_exclusive(src(44_100, 24, SampleFormat::S24), &supported).unwrap_err();
        assert!(matches!(err, LyraError::Device(_)));
    }

    #[test]
    fn rejects_float_source() {
        let supported = [fmt(48_000, SampleFormat::S32, 32)];
        let err = negotiate_exclusive(src(48_000, 32, SampleFormat::F32), &supported).unwrap_err();
        assert!(matches!(err, LyraError::UnsupportedFormat(_)));
    }

    #[test]
    fn rejects_container_that_advertises_fewer_valid_bits_than_source() {
        // 32-bit source, device only offers 24-in-32. Advertising 24 valid bits
        // would make the DAC drop the low 8 source bits, so this must fail rather
        // than silently lose precision.
        let supported = [fmt(192_000, SampleFormat::S32, 24)];
        let err = negotiate_exclusive(src(192_000, 32, SampleFormat::S32), &supported).unwrap_err();
        assert!(matches!(err, LyraError::Device(_)));
    }

    #[test]
    fn thirty_two_bit_needs_full_s32() {
        let supported = [
            fmt(192_000, SampleFormat::S32, 24),
            fmt(192_000, SampleFormat::S32, 32),
        ];
        let n = negotiate_exclusive(src(192_000, 32, SampleFormat::S32), &supported).unwrap();
        assert_eq!(n.spec.format, SampleFormat::S32);
        assert_eq!(n.request_valid_bits, 32);
        assert!(!n.padded);
    }
}
