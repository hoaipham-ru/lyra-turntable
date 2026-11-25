//! Raw interleaved PCM: container word sizes, sample unpacking and the
//! ADR-004 padding rule (recovering valid-bit-justified samples from a wider
//! container).

use anyhow::{bail, Result};
use lyra_core::SampleFormat;

/// Number of bits in the storage container of a [`SampleFormat`].
#[must_use]
pub fn container_bits(fmt: SampleFormat) -> u32 {
    match fmt {
        SampleFormat::S16 => 16,
        SampleFormat::S24 => 24,
        SampleFormat::S32 | SampleFormat::F32 => 32,
    }
}

/// Pick the narrowest integer container that losslessly holds `bits` valid bits.
///
/// Mirrors the golden-decode rule: `<= 16` → s16le, `<= 24` → s24le (packed
/// 3-byte), otherwise s32le. This matches `flac -d --force-raw-format` and
/// `ffmpeg -c:a pcm_s{16,24,32}le`.
#[must_use]
pub fn format_for_bits(bits: u32) -> SampleFormat {
    if bits <= 16 {
        SampleFormat::S16
    } else if bits <= 24 {
        SampleFormat::S24
    } else {
        SampleFormat::S32
    }
}

/// ffmpeg PCM encoder name for a container format (little-endian).
#[must_use]
pub fn ffmpeg_pcm_codec(fmt: SampleFormat) -> &'static str {
    match fmt {
        SampleFormat::S16 => "pcm_s16le",
        SampleFormat::S24 => "pcm_s24le",
        SampleFormat::S32 => "pcm_s32le",
        SampleFormat::F32 => "pcm_f32le",
    }
}

/// Read one signed integer sample from the little-endian container at `off`.
///
/// # Errors
/// Returns an error for [`SampleFormat::F32`]: IEEE-754 float samples are not
/// integers and must not be reinterpreted as such (that would arithmetic-shift
/// the raw float bit pattern and produce garbage). F32 is a bit-perfect contract
/// type later phases will feed here; it needs its own float-aware unpack path,
/// not this one.
fn read_container(bytes: &[u8], fmt: SampleFormat, off: usize) -> Result<i32> {
    match fmt {
        SampleFormat::S16 => {
            let v = i16::from_le_bytes([bytes[off], bytes[off + 1]]);
            Ok(i32::from(v))
        }
        SampleFormat::S24 => {
            // 3-byte little-endian, sign-extend bit 23 into bits 24..31.
            let raw =
                (i32::from(bytes[off])) | (i32::from(bytes[off + 1]) << 8) | (i32::from(bytes[off + 2]) << 16);
            Ok((raw << 8) >> 8)
        }
        SampleFormat::S32 => {
            Ok(i32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]))
        }
        SampleFormat::F32 => bail!(
            "F32 PCM is not supported by pcm::unpack: IEEE-754 float samples cannot be \
             reinterpreted as integer samples. Use a float-aware unpack path instead."
        ),
    }
}

/// Unpack a raw interleaved PCM byte buffer into valid-bit-justified `i32`
/// samples.
///
/// The ADR-004 padding rule lives here. A container that is wider than the
/// valid bit depth (e.g. a 24-bit source carried 24-in-32) stores each sample
/// shifted up by `container_bits - bits_valid`; we shift it back down so two
/// streams declared at the same `bits_valid` compare directly, regardless of
/// whether one was packed 24-bit and the other 24-in-32.
///
/// Trailing bytes that do not form a whole sample are ignored.
///
/// # Errors
/// Returns an error for [`SampleFormat::F32`], which carries IEEE-754 float
/// samples that cannot be unpacked as integers here (see [`read_container`]).
/// The `s16`/`s24`/`s32` paths never error.
pub fn unpack(bytes: &[u8], fmt: SampleFormat, bits_valid: u32) -> Result<Vec<i32>> {
    let stride = fmt.bytes_per_sample();
    let shift = container_bits(fmt).saturating_sub(bits_valid);
    let n = bytes.len() / stride;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let s = read_container(bytes, fmt, i * stride)?;
        out.push(s >> shift);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_selection_by_bits() {
        assert_eq!(format_for_bits(16), SampleFormat::S16);
        assert_eq!(format_for_bits(8), SampleFormat::S16);
        assert_eq!(format_for_bits(24), SampleFormat::S24);
        assert_eq!(format_for_bits(20), SampleFormat::S24);
        assert_eq!(format_for_bits(32), SampleFormat::S32);
    }

    #[test]
    fn unpack_s16_roundtrip() {
        let samples: [i16; 4] = [0, 1, -1, i16::MIN];
        let mut bytes = Vec::new();
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let got = unpack(&bytes, SampleFormat::S16, 16).unwrap();
        assert_eq!(got, vec![0, 1, -1, i32::from(i16::MIN)]);
    }

    #[test]
    fn unpack_s24_sign_extends() {
        // -1 packed as 3 little-endian bytes: 0xFF 0xFF 0xFF.
        let bytes = [0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x80];
        let got = unpack(&bytes, SampleFormat::S24, 24).unwrap();
        assert_eq!(got, vec![-1, -(1 << 23)]);
    }

    #[test]
    fn adr004_24_in_32_matches_packed_24() {
        // ADR-004: a 24-bit value carried 24-in-32 is stored << 8.
        let value_24: i32 = 0x12_3456; // a 24-bit sample value
        // packed 24-bit little-endian
        let packed = [0x56u8, 0x34, 0x12];
        // 24-in-32: value << 8, little-endian
        let in32 = (value_24 << 8).to_le_bytes();

        let from_packed = unpack(&packed, SampleFormat::S24, 24).unwrap();
        let from_32 = unpack(&in32, SampleFormat::S32, 24).unwrap();
        assert_eq!(from_packed, vec![value_24]);
        assert_eq!(from_32, vec![value_24]);
        assert_eq!(from_packed, from_32);
    }

    #[test]
    fn unpack_rejects_f32() {
        // F32 carries IEEE-754 float samples; unpacking them as integers would
        // silently produce garbage, so it must be rejected with a clear error.
        let bytes = 1.0f32.to_le_bytes();
        let err = unpack(&bytes, SampleFormat::F32, 32)
            .expect_err("F32 must be rejected by unpack");
        let msg = err.to_string();
        assert!(msg.contains("F32"), "error should name F32, got: {msg}");
    }
}
