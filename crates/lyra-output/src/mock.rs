//! An in-process mock output device.
//!
//! The real WASAPI device cannot be opened in an automated test (it would make
//! noise and needs hardware), so the format-negotiation logic and the
//! [`OutputDevice`] contract are exercised against this mock instead
//! (S04 "Mock … for tests"). It records the exact bytes that would be handed to
//! the device, so tests can assert the padding rule (ADR-004) end to end.

use lyra_core::{
    DeviceCaps, Frames, LyraError, OutputDevice, OutputMode, PcmSpec, Result, SampleFormat,
    StreamSpec,
};

use crate::negotiate::{negotiate_exclusive, ExclusiveFormat, Negotiated};
use crate::pad::pad_frames;

/// Configurable fake device for tests.
#[derive(Debug, Clone)]
pub struct MockDevice {
    /// Formats this fake accepts in exclusive mode.
    pub supported: Vec<ExclusiveFormat>,
    /// Shared-mode mix format, if the fake supports shared mode.
    pub mix: Option<PcmSpec>,
    /// Friendly name reported through [`OutputDevice::caps`].
    pub name: String,
    source: Option<PcmSpec>,
    negotiated: Option<Negotiated>,
    bit_perfect: bool,
    running: bool,
    scratch: Vec<u8>,
    /// Every byte that would have reached the device buffer, in device format.
    pub captured: Vec<u8>,
}

impl MockDevice {
    /// Build a mock that accepts `supported` in exclusive mode.
    #[must_use]
    pub fn new(name: impl Into<String>, supported: Vec<ExclusiveFormat>) -> Self {
        Self {
            supported,
            mix: None,
            name: name.into(),
            source: None,
            negotiated: None,
            bit_perfect: false,
            running: false,
            scratch: Vec::new(),
            captured: Vec::new(),
        }
    }

    /// A mock shaped like the dev laptop codec: 48 kHz only, 24-in-32 and S32.
    #[must_use]
    pub fn laptop_codec() -> Self {
        Self::new(
            "Mock Senary Speakers (48 kHz)",
            vec![
                ExclusiveFormat { rate: 48_000, format: SampleFormat::S32, valid_bits: 24 },
                ExclusiveFormat { rate: 48_000, format: SampleFormat::S32, valid_bits: 32 },
            ],
        )
    }

    /// The format actually negotiated at the last [`OutputDevice::open`].
    #[must_use]
    pub fn negotiated(&self) -> Option<Negotiated> {
        self.negotiated
    }
}

impl OutputDevice for MockDevice {
    fn open(&mut self, spec: StreamSpec, mode: OutputMode) -> Result<StreamSpec> {
        let pcm = match spec {
            StreamSpec::Pcm(p) => p,
            StreamSpec::Dsd(_) => {
                return Err(LyraError::UnsupportedFormat("mock device is PCM only".into()))
            }
        };
        match mode {
            OutputMode::WasapiExclusive => {
                let neg = negotiate_exclusive(pcm, &self.supported)?;
                self.negotiated = Some(neg);
                self.source = Some(pcm);
                self.bit_perfect = true;
                Ok(StreamSpec::Pcm(neg.spec))
            }
            OutputMode::WasapiShared => {
                let mix = self
                    .mix
                    .ok_or_else(|| LyraError::Device("mock device has no shared mix".into()))?;
                // Shared mode is bit-perfect only if the mix matches the source.
                self.bit_perfect = mix == pcm;
                self.source = Some(mix);
                self.negotiated = None;
                Ok(StreamSpec::Pcm(mix))
            }
            OutputMode::Asio { .. } => {
                Err(LyraError::UnsupportedFormat("ASIO is a P5 feature".into()))
            }
        }
    }

    fn start(&mut self) -> Result<()> {
        if self.source.is_none() {
            return Err(LyraError::InvalidState("start before open".into()));
        }
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.running = false;
        Ok(())
    }

    fn write(&mut self, frames: &Frames<'_>) -> Result<usize> {
        if !self.running {
            return Err(LyraError::InvalidState("write before start".into()));
        }
        let src = match frames.spec {
            StreamSpec::Pcm(p) => p,
            StreamSpec::Dsd(_) => {
                return Err(LyraError::UnsupportedFormat("mock device is PCM only".into()))
            }
        };
        let in_bytes = &frames.data[..frames.frames * src.bytes_per_frame()];
        match self.negotiated {
            // Exclusive: apply the container padding so `captured` holds the exact
            // device bytes (padding rule, ADR-004).
            Some(neg) => {
                pad_frames(src, neg.spec.format, in_bytes, &mut self.scratch);
                self.captured.extend_from_slice(&self.scratch);
            }
            // Shared: record as-is.
            None => self.captured.extend_from_slice(in_bytes),
        }
        Ok(frames.frames)
    }

    fn caps(&self) -> DeviceCaps {
        DeviceCaps {
            name: self.name.clone(),
            exclusive: !self.supported.is_empty(),
            shared: self.mix.is_some(),
            asio: false,
            dop: false,
            native_dsd: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(rate: u32, bits: u8, format: SampleFormat) -> PcmSpec {
        PcmSpec { rate, channels: 2, bits_valid: bits, format }
    }

    #[test]
    fn open_negotiates_and_reports_padded_container() {
        let mut dev = MockDevice::laptop_codec();
        let neg = dev
            .open(StreamSpec::Pcm(pcm(48_000, 16, SampleFormat::S16)), OutputMode::WasapiExclusive)
            .unwrap();
        match neg {
            StreamSpec::Pcm(p) => {
                assert_eq!(p.format, SampleFormat::S32);
                assert_eq!(p.bits_valid, 16);
            }
            StreamSpec::Dsd(_) => panic!("expected PCM"),
        }
    }

    #[test]
    fn write_captures_padded_device_bytes() {
        let mut dev = MockDevice::laptop_codec();
        dev.open(StreamSpec::Pcm(pcm(48_000, 16, SampleFormat::S16)), OutputMode::WasapiExclusive)
            .unwrap();
        dev.start().unwrap();
        // One stereo frame: L=0x1122, R=0x3344 (LE).
        let data = [0x22, 0x11, 0x44, 0x33];
        let frames = Frames {
            spec: StreamSpec::Pcm(pcm(48_000, 16, SampleFormat::S16)),
            data: &data,
            frames: 1,
        };
        let n = dev.write(&frames).unwrap();
        assert_eq!(n, 1);
        // Each 16-bit sample MSB-aligned in 32 bits, LSBs zero.
        assert_eq!(dev.captured, [0x00, 0x00, 0x22, 0x11, 0x00, 0x00, 0x44, 0x33]);
    }

    #[test]
    fn write_before_start_errors() {
        let mut dev = MockDevice::laptop_codec();
        dev.open(StreamSpec::Pcm(pcm(48_000, 24, SampleFormat::S24)), OutputMode::WasapiExclusive)
            .unwrap();
        let data = [0u8; 6];
        let frames = Frames {
            spec: StreamSpec::Pcm(pcm(48_000, 24, SampleFormat::S24)),
            data: &data,
            frames: 1,
        };
        assert!(matches!(dev.write(&frames), Err(LyraError::InvalidState(_))));
    }

    #[test]
    fn shared_mode_flags_not_bit_perfect_on_mismatch() {
        let mut dev = MockDevice::new("mock", vec![]);
        dev.mix = Some(pcm(48_000, 24, SampleFormat::S32));
        // Source is 44.1 kHz; shared mix is 48 kHz -> not bit-perfect.
        dev.open(StreamSpec::Pcm(pcm(44_100, 16, SampleFormat::S16)), OutputMode::WasapiShared)
            .unwrap();
        assert!(!dev.bit_perfect);
    }

    #[test]
    fn caps_reports_exclusive() {
        let dev = MockDevice::laptop_codec();
        let caps = dev.caps();
        assert!(caps.exclusive);
        assert!(!caps.asio);
    }
}
