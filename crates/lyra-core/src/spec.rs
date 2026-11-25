//! Sample and stream descriptions, plus the byte-layout and DoP-rate helpers.

/// Interleaved PCM container sample format.
///
/// This names the *container* word size used to carry each sample, not the
/// number of meaningful bits (that is [`PcmSpec::bits_valid`]). For example a
/// 16-bit source carried in a 32-bit container is [`SampleFormat::S32`] with
/// `bits_valid = 16`; the extra low bits are zero-padded (ADR-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// Signed 16-bit integer, 2 bytes per sample.
    S16,
    /// Signed 24-bit integer packed in 3 bytes per sample.
    S24,
    /// Signed 32-bit integer, 4 bytes per sample.
    S32,
    /// 32-bit IEEE-754 float, 4 bytes per sample.
    F32,
}

impl SampleFormat {
    /// Number of bytes one sample of this format occupies in the container.
    ///
    /// This is the container size (`S24` is packed in 3 bytes), independent of
    /// how many bits are actually valid.
    #[must_use]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            SampleFormat::S16 => 2,
            SampleFormat::S24 => 3,
            SampleFormat::S32 => 4,
            SampleFormat::F32 => 4,
        }
    }
}

/// A PCM stream description: rate, channel count, valid bits and container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PcmSpec {
    /// Sample rate in Hz (frames per second), e.g. `44_100`.
    pub rate: u32,
    /// Number of interleaved channels (1 = mono, 2 = stereo).
    pub channels: u16,
    /// Count of meaningful bits in each sample, `<=` the container width.
    pub bits_valid: u8,
    /// Container word size of each sample.
    pub format: SampleFormat,
}

impl PcmSpec {
    /// Bytes occupied by one frame (one sample for every channel).
    ///
    /// `bytes_per_sample(format) * channels`.
    #[must_use]
    pub const fn bytes_per_frame(self) -> usize {
        self.format.bytes_per_sample() * self.channels as usize
    }
}

/// DSD bit rate, expressed as a multiple of the CD base rate (44.1 kHz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DsdRate {
    /// DSD64 — 64 × 44.1 kHz = 2.8224 MHz.
    Dsd64,
    /// DSD128 — 128 × 44.1 kHz = 5.6448 MHz.
    Dsd128,
    /// DSD256 — 256 × 44.1 kHz = 11.2896 MHz.
    Dsd256,
    /// DSD512 — 512 × 44.1 kHz = 22.5792 MHz.
    Dsd512,
}

impl DsdRate {
    /// The raw DSD bit rate in Hz (one-bit samples per second).
    #[must_use]
    pub const fn hz(self) -> u32 {
        match self {
            DsdRate::Dsd64 => 2_822_400,
            DsdRate::Dsd128 => 5_644_800,
            DsdRate::Dsd256 => 11_289_600,
            DsdRate::Dsd512 => 22_579_200,
        }
    }

    /// The PCM sample rate that carries this DSD stream over DoP.
    ///
    /// DoP (DSD-over-PCM) packs 16 DSD bits per channel into one 24-bit PCM
    /// sample, so the carrier PCM rate is the DSD rate divided by 16
    /// (e.g. DSD64 → 2.8224 MHz / 16 = 176.4 kHz).
    #[must_use]
    pub const fn dop_pcm_rate(self) -> u32 {
        self.hz() / 16
    }
}

/// A DSD stream description: rate, channel count and bit order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DsdSpec {
    /// DSD bit rate.
    pub rate: DsdRate,
    /// Number of channels.
    pub channels: u16,
    /// `true` if the first bit in each byte is the most-significant (DSF is
    /// LSB-first, DFF is MSB-first).
    pub msb_first: bool,
}

/// A stream is either PCM or DSD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamSpec {
    /// Linear PCM.
    Pcm(PcmSpec),
    /// 1-bit DSD.
    Dsd(DsdSpec),
}

impl StreamSpec {
    /// Number of channels, regardless of stream kind.
    #[must_use]
    pub const fn channels(self) -> u16 {
        match self {
            StreamSpec::Pcm(p) => p.channels,
            StreamSpec::Dsd(d) => d.channels,
        }
    }
}

/// A borrowed block of interleaved samples handed between crates.
///
/// `data` is the raw interleaved byte buffer and `frames` is how many frames it
/// contains. For PCM, `data.len()` equals `frames * spec.bytes_per_frame()`.
/// Nothing in this crate allocates or copies the buffer; it is borrowed from the
/// producer for the lifetime `'a`.
#[derive(Debug, Clone, Copy)]
pub struct Frames<'a> {
    /// The format of the samples in `data`.
    pub spec: StreamSpec,
    /// Raw interleaved sample bytes.
    pub data: &'a [u8],
    /// Number of frames represented by `data`.
    pub frames: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_per_sample_by_format() {
        assert_eq!(SampleFormat::S16.bytes_per_sample(), 2);
        assert_eq!(SampleFormat::S24.bytes_per_sample(), 3);
        assert_eq!(SampleFormat::S32.bytes_per_sample(), 4);
        assert_eq!(SampleFormat::F32.bytes_per_sample(), 4);
    }

    #[test]
    fn bytes_per_frame_by_spec() {
        let stereo16 = PcmSpec { rate: 44_100, channels: 2, bits_valid: 16, format: SampleFormat::S16 };
        assert_eq!(stereo16.bytes_per_frame(), 4);

        // 24-bit source padded into a 32-bit container, stereo.
        let stereo24in32 = PcmSpec { rate: 96_000, channels: 2, bits_valid: 24, format: SampleFormat::S32 };
        assert_eq!(stereo24in32.bytes_per_frame(), 8);

        // Packed 24-bit, stereo.
        let stereo24 = PcmSpec { rate: 96_000, channels: 2, bits_valid: 24, format: SampleFormat::S24 };
        assert_eq!(stereo24.bytes_per_frame(), 6);

        let mono_f32 = PcmSpec { rate: 48_000, channels: 1, bits_valid: 32, format: SampleFormat::F32 };
        assert_eq!(mono_f32.bytes_per_frame(), 4);

        let surround32 = PcmSpec { rate: 192_000, channels: 6, bits_valid: 32, format: SampleFormat::S32 };
        assert_eq!(surround32.bytes_per_frame(), 24);
    }

    #[test]
    fn dsd_raw_rates() {
        assert_eq!(DsdRate::Dsd64.hz(), 2_822_400);
        assert_eq!(DsdRate::Dsd128.hz(), 5_644_800);
        assert_eq!(DsdRate::Dsd256.hz(), 11_289_600);
        assert_eq!(DsdRate::Dsd512.hz(), 22_579_200);
    }

    #[test]
    fn dsd_to_dop_pcm_rate() {
        // DoP carrier PCM rate = DSD rate / 16.
        assert_eq!(DsdRate::Dsd64.dop_pcm_rate(), 176_400);
        assert_eq!(DsdRate::Dsd128.dop_pcm_rate(), 352_800);
        assert_eq!(DsdRate::Dsd256.dop_pcm_rate(), 705_600);
        assert_eq!(DsdRate::Dsd512.dop_pcm_rate(), 1_411_200);
    }

    #[test]
    fn stream_channels() {
        let pcm = StreamSpec::Pcm(PcmSpec { rate: 44_100, channels: 2, bits_valid: 16, format: SampleFormat::S16 });
        let dsd = StreamSpec::Dsd(DsdSpec { rate: DsdRate::Dsd64, channels: 1, msb_first: true });
        assert_eq!(pcm.channels(), 2);
        assert_eq!(dsd.channels(), 1);
    }
}
