//! Native-PCM container selection (ADR-004) and sample packing.
//!
//! The decoder emits samples with **no conversion**: the source sample rate and
//! channel count are preserved, and each sample is packed into the smallest
//! container that losslessly holds `bits_valid`. For 24-bit sources the container
//! is chosen by [`Container24`]:
//!
//! * [`Container24::Packed`] (default) — 24 bits packed in 3 bytes (`S24`),
//!   byte-identical to `flac -d --force-raw-format` and `ffmpeg … pcm_s24le`
//!   golden output.
//! * [`Container24::LeftJustifiedS32`] — 24 bits carried in a 32-bit word,
//!   left-justified (value `<< 8`, LSBs zero-padded), per the ADR-004 widening
//!   rule. `bits_valid` stays 24 so a consumer can recover the native value.

use lyra_core::SampleFormat;

/// How a 24-bit source is carried in the output container (ADR-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Container24 {
    /// 24 bits packed in 3 bytes (`SampleFormat::S24`). Default.
    #[default]
    Packed,
    /// 24 bits in a 32-bit word, left-justified with zero-padded LSBs
    /// (`SampleFormat::S32`, `bits_valid = 24`).
    LeftJustifiedS32,
}

/// The chosen output container plus the left-shift applied when storing each
/// native (LSB-justified) sample value into that container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// Output container word size.
    pub format: SampleFormat,
    /// Count of meaningful bits in each stored sample.
    pub bits_valid: u8,
    /// Left shift applied to the native value before it is stored. `0` for the
    /// packed/narrowest containers; `container_bits - bits_valid` when
    /// left-justifying into a wider container.
    pub store_shift: u32,
    /// `true` when samples are IEEE-754 floats carried verbatim (no int packing).
    pub is_float: bool,
}

impl Target {
    /// Choose the output container for a `bits_valid`-bit integer source.
    #[must_use]
    pub fn choose_int(bits_valid: u8, container24: Container24) -> Target {
        let (format, store_shift) = if bits_valid <= 16 {
            (SampleFormat::S16, 0)
        } else if bits_valid <= 24 {
            match container24 {
                Container24::Packed => (SampleFormat::S24, 0),
                Container24::LeftJustifiedS32 => (SampleFormat::S32, 32 - u32::from(bits_valid)),
            }
        } else {
            (SampleFormat::S32, 0)
        };
        Target { format, bits_valid, store_shift, is_float: false }
    }

    /// A float pass-through target (`F32`, carried verbatim).
    #[must_use]
    pub fn float32() -> Target {
        Target { format: SampleFormat::F32, bits_valid: 32, store_shift: 0, is_float: true }
    }
}

/// Append one native (LSB-justified) integer sample to `out` in `target`'s
/// container layout.
#[inline]
pub fn push_int(out: &mut Vec<u8>, native: i32, target: Target) {
    let v = native << target.store_shift;
    match target.format {
        SampleFormat::S16 => out.extend_from_slice(&(v as i16).to_le_bytes()),
        SampleFormat::S24 => {
            let b = v.to_le_bytes();
            out.push(b[0]);
            out.push(b[1]);
            out.push(b[2]);
        }
        SampleFormat::S32 => out.extend_from_slice(&v.to_le_bytes()),
        // Float targets never reach here (handled on the float path).
        SampleFormat::F32 => out.extend_from_slice(&(v as f32).to_le_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_narrowest_container() {
        assert_eq!(Target::choose_int(16, Container24::Packed).format, SampleFormat::S16);
        assert_eq!(Target::choose_int(8, Container24::Packed).format, SampleFormat::S16);
        assert_eq!(Target::choose_int(24, Container24::Packed).format, SampleFormat::S24);
        assert_eq!(Target::choose_int(24, Container24::Packed).store_shift, 0);
        assert_eq!(Target::choose_int(32, Container24::Packed).format, SampleFormat::S32);
    }

    #[test]
    fn left_justified_24_in_32() {
        let t = Target::choose_int(24, Container24::LeftJustifiedS32);
        assert_eq!(t.format, SampleFormat::S32);
        assert_eq!(t.bits_valid, 24);
        assert_eq!(t.store_shift, 8);
    }

    #[test]
    fn packs_s16_le() {
        let mut out = Vec::new();
        push_int(&mut out, -1, Target::choose_int(16, Container24::Packed));
        assert_eq!(out, vec![0xFF, 0xFF]);
    }

    #[test]
    fn packs_s24_le_three_bytes() {
        let mut out = Vec::new();
        // 0x123456 packed little-endian.
        push_int(&mut out, 0x12_3456, Target::choose_int(24, Container24::Packed));
        assert_eq!(out, vec![0x56, 0x34, 0x12]);
    }

    #[test]
    fn left_justified_matches_shifted_value() {
        let mut out = Vec::new();
        let t = Target::choose_int(24, Container24::LeftJustifiedS32);
        push_int(&mut out, 0x12_3456, t);
        // value << 8 stored as 32-bit LE.
        assert_eq!(out, (0x12_3456i32 << 8).to_le_bytes().to_vec());
    }
}
