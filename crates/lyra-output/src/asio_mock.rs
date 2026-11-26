//! An in-process **mock ASIO driver** and the [`OutputDevice`] that drives it
//! (P5.4, S04 "Mock ASIO driver for tests", ADR-009).
//!
//! No ASIO hardware or SDK exists on the dev laptop (ADR-009), so the native-DSD
//! and DoP-over-ASIO paths are built and tested against this fake instead. The
//! mock models just the driver calls Lyra uses:
//! `init` / `ASIOGetChannels` / `ASIOGetBufferSize` / `ASIOCanSampleRate`
//! (for PCM **and**, under `kASIODSDFormat`, for DSD) / `ASIOSetIoFormat` /
//! the buffer-switch callback. It records every byte handed to it so tests can
//! assert bit-exactness (like [`crate::mock::MockDevice::captured`]).
//!
//! # The three DSD paths (ADR-009), as this device sees them
//! - **Native DSD** — `OutputMode::Asio { native_dsd: true }` on a driver that
//!   reports `kASIODSDFormat` at the file's DSD rate: the raw DSD bytes are passed
//!   straight through to the driver buffer, **no PCM conversion**. Captured bytes
//!   equal the source DSD bytes.
//! - **DoP over ASIO** — a DoP-capable but not native-DSD driver: the device
//!   produces a 24-bit PCM **carrier** format; the engine feeds it DoP-packed PCM
//!   frames (from `lyra_dsd::DopPacker`, written in parallel — see the integration
//!   seam below). The device never inspects or rewrites the marker bytes.
//! - **DSD → PCM** — neither available: a DSD spec is **rejected** here with a
//!   clear error. The decimation path lives in `lyra-dsd` (P5.6) and engine
//!   routing in P5.5/P3, not in the output service.
//!
//! # Bit order (native DSD)
//! A driver asks for a specific DSD **bit order** (DSF is LSB-first, DFF is
//! MSB-first; a given ASIO driver wants one of them). This device reports the order
//! it wants via [`AsioMockDriver::dsd_msb_first`] and passes bytes through
//! unchanged — presenting the DSD bytes in that order is the decoder's / engine's
//! job (`lyra-dsd`, the integration seam). The native-DSD tests set the source
//! order equal to the driver's requested order and assert a byte-exact copy.
//!
//! # Integration seam (DoP packer)
//! This module does **not** depend on `lyra_dsd::DopPacker`'s API yet. It only
//! produces the carrier [`PcmSpec`] and copies whatever DoP-packed PCM bytes it is
//! given. Tests exercise the path with a small stand-in DoP buffer. Engine wiring
//! (choosing native → DoP → convert from [`OutputDevice::caps`]) is P5.5/P3.

use lyra_core::{
    DeviceCaps, DsdRate, DsdSpec, Frames, LyraError, OutputDevice, OutputMode, PcmSpec, Result,
    SampleFormat, StreamSpec,
};

use crate::negotiate::{negotiate_exclusive, ExclusiveFormat, Negotiated};
use crate::pad::pad_frames;

/// The I/O transport an ASIO stream is set to with `ASIOSetIoFormat`.
///
/// Mirrors the two `ASIOIoFormatType`s Lyra uses (`asio-sys` exposes the same two
/// constants): ordinary PCM, and native 1-bit DSD (`kASIODSDFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsioIoFormat {
    /// `kASIOPCMFormat` — linear PCM (also the DoP carrier).
    Pcm,
    /// `kASIODSDFormat` — native DSD transport.
    Dsd,
}

/// A configurable fake ASIO driver for tests.
///
/// Build one with [`AsioMockDriver::pcm_only`], [`AsioMockDriver::pcm_dop`] or
/// [`AsioMockDriver::native_dsd`], then wrap it in an [`AsioMockDevice`].
#[derive(Debug, Clone)]
pub struct AsioMockDriver {
    /// Driver name reported through [`OutputDevice::caps`].
    pub name: String,
    /// PCM formats the driver accepts (what `ASIOCanSampleRate` + the channel's
    /// sample type would allow). Negotiation picks a container from these, exactly
    /// as the WASAPI path does.
    pub pcm_formats: Vec<ExclusiveFormat>,
    /// DSD rates the driver reports under `kASIODSDFormat` (`ASIOCanSampleRate`
    /// while the I/O format is DSD). Empty ⇒ native DSD unsupported.
    pub dsd_rates: Vec<DsdRate>,
    /// The DSD **bit order** the driver wants for native DSD (`true` = MSB-first).
    /// Reported to the engine; this device does not reorder bits itself.
    pub dsd_msb_first: bool,
    /// Whether this device is known DoP-capable. ADR-009: never assumed for an
    /// unknown device; sending DoP to a device not on the known-good list plays
    /// loud noise, so this is an explicit opt-in.
    pub dop_capable: bool,
    /// Channel count the driver exposes (`ASIOGetChannels`).
    pub channels: u16,
    /// Preferred buffer size in frames (`ASIOGetBufferSize`).
    pub buffer_frames: usize,
    /// I/O format currently selected (`ASIOSetIoFormat`).
    io_format: AsioIoFormat,
    /// Every byte received in buffer-switch callbacks, in device order. The
    /// bit-perfect proof for the mocked paths (mirrors `MockDevice.captured`).
    pub captured: Vec<u8>,
}

/// Standard PCM candidates an ASIO driver offers at one rate: packed 24-in-3,
/// 24-in-32, full S32 and S16 (same grid the WASAPI path probes).
fn pcm_grid(rates: &[u32]) -> Vec<ExclusiveFormat> {
    let mut out = Vec::new();
    for &rate in rates {
        out.push(ExclusiveFormat { rate, format: SampleFormat::S24, valid_bits: 24 });
        out.push(ExclusiveFormat { rate, format: SampleFormat::S32, valid_bits: 24 });
        out.push(ExclusiveFormat { rate, format: SampleFormat::S32, valid_bits: 32 });
        out.push(ExclusiveFormat { rate, format: SampleFormat::S16, valid_bits: 16 });
    }
    out
}

impl AsioMockDriver {
    /// (a) A **PCM-only** ASIO driver: the usual rate grid, no DSD, not DoP.
    #[must_use]
    pub fn pcm_only(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            pcm_formats: pcm_grid(&[44_100, 48_000, 88_200, 96_000, 176_400, 192_000]),
            dsd_rates: Vec::new(),
            dsd_msb_first: true,
            dop_capable: false,
            channels: 2,
            buffer_frames: 1024,
            io_format: AsioIoFormat::Pcm,
            captured: Vec::new(),
        }
    }

    /// (b) A **PCM + DoP-capable** ASIO driver: accepts 24-bit at the DoP carrier
    /// rates (176.4 / 352.8 / 705.6 kHz) and is flagged DoP-capable, but has no
    /// native DSD transport.
    #[must_use]
    pub fn pcm_dop(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            pcm_formats: pcm_grid(&[44_100, 48_000, 88_200, 96_000, 176_400, 352_800, 705_600]),
            dsd_rates: Vec::new(),
            dsd_msb_first: true,
            dop_capable: true,
            channels: 2,
            buffer_frames: 1024,
            io_format: AsioIoFormat::Pcm,
            captured: Vec::new(),
        }
    }

    /// (c) A **native-DSD-capable** ASIO driver: reports `kASIODSDFormat` at
    /// `dsd_rates`, plus the normal PCM grid. `msb_first` is the bit order it asks
    /// for.
    #[must_use]
    pub fn native_dsd(name: impl Into<String>, dsd_rates: Vec<DsdRate>, msb_first: bool) -> Self {
        Self {
            name: name.into(),
            pcm_formats: pcm_grid(&[44_100, 48_000, 88_200, 96_000, 176_400, 192_000]),
            dsd_rates,
            dsd_msb_first: msb_first,
            dop_capable: false,
            channels: 2,
            buffer_frames: 1024,
            io_format: AsioIoFormat::Pcm,
            captured: Vec::new(),
        }
    }

    /// `ASIOCanSampleRate` for native DSD: `true` if the driver reports this DSD
    /// rate under `kASIODSDFormat`.
    #[must_use]
    pub fn can_dsd_rate(&self, rate: DsdRate) -> bool {
        self.dsd_rates.contains(&rate)
    }

    /// `ASIOSetIoFormat`: select PCM or DSD transport. Selecting DSD on a driver
    /// with no `kASIODSDFormat` support fails, exactly as a real driver would.
    pub fn set_io_format(&mut self, fmt: AsioIoFormat) -> Result<()> {
        if fmt == AsioIoFormat::Dsd && self.dsd_rates.is_empty() {
            return Err(LyraError::Device(format!(
                "ASIO driver '{}' does not support kASIODSDFormat",
                self.name
            )));
        }
        self.io_format = fmt;
        Ok(())
    }

    /// The I/O format currently selected.
    #[must_use]
    pub fn io_format(&self) -> AsioIoFormat {
        self.io_format
    }

    /// The buffer-switch callback: the driver receives `bytes` for one buffer and
    /// records them. In the real backend this copies into the ASIO buffer on the
    /// driver's callback thread with no allocation; here it just captures.
    pub fn buffer_switch(&mut self, bytes: &[u8]) {
        self.captured.extend_from_slice(bytes);
    }
}

/// What the device is currently streaming, decided at [`OutputDevice::open`].
#[derive(Debug, Clone, Copy)]
enum AsioStream {
    /// Linear PCM — either plain PCM playback or the DoP carrier. Carries the
    /// negotiated container so [`OutputDevice::write`] applies the padding rule.
    Pcm { source: PcmSpec, neg: Negotiated },
    /// Native DSD passthrough.
    NativeDsd { spec: DsdSpec },
}

/// An [`OutputDevice`] backed by a mocked ASIO driver.
#[derive(Debug, Clone)]
pub struct AsioMockDevice {
    /// The fake driver. `driver.captured` is the bit-perfect proof.
    pub driver: AsioMockDriver,
    stream: Option<AsioStream>,
    running: bool,
    scratch: Vec<u8>,
}

impl AsioMockDevice {
    /// Wrap a mock driver in an output device.
    #[must_use]
    pub fn new(driver: AsioMockDriver) -> Self {
        Self { driver, stream: None, running: false, scratch: Vec::new() }
    }

    /// The DoP carrier PCM format for a DSD stream: 24 valid bits at the DoP
    /// carrier rate (DSD rate / 16), stereo as the DSD file. The container is then
    /// negotiated against the driver's PCM formats (packed 24-in-3 preferred, so a
    /// DoP word — marker byte included — survives byte-for-byte).
    #[must_use]
    pub fn dop_carrier_spec(dsd: DsdSpec) -> PcmSpec {
        PcmSpec {
            rate: dsd.rate.dop_pcm_rate(),
            channels: dsd.channels,
            bits_valid: 24,
            format: SampleFormat::S24,
        }
    }

    /// Open a DSD stream, choosing native DSD or DoP per `native_dsd` and the
    /// driver's capabilities. Separated out to keep [`OutputDevice::open`] flat.
    fn open_dsd(&mut self, dsd: DsdSpec, native_dsd: bool) -> Result<StreamSpec> {
        if native_dsd {
            // Native DSD requested: require kASIODSDFormat at this rate.
            if !self.driver.can_dsd_rate(dsd.rate) {
                return Err(LyraError::UnsupportedFormat(format!(
                    "ASIO driver '{}' does not report native DSD at {:?}",
                    self.driver.name, dsd.rate
                )));
            }
            self.driver.set_io_format(AsioIoFormat::Dsd)?;
            // Report the stream in the bit order the driver asks for; the engine /
            // lyra-dsd must deliver bytes in that order (integration seam).
            let negotiated = DsdSpec { msb_first: self.driver.dsd_msb_first, ..dsd };
            self.stream = Some(AsioStream::NativeDsd { spec: negotiated });
            return Ok(StreamSpec::Dsd(negotiated));
        }

        // Not native: DoP is the only remaining DSD transport here. DSD→PCM
        // decimation is lyra-dsd (P5.6), never this crate.
        if !self.driver.dop_capable {
            return Err(LyraError::UnsupportedFormat(format!(
                "ASIO driver '{}' is PCM only: no native DSD and not DoP-capable; \
                 DSD must be converted to PCM upstream (lyra-dsd)",
                self.driver.name
            )));
        }
        self.driver.set_io_format(AsioIoFormat::Pcm)?;
        let carrier = Self::dop_carrier_spec(dsd);
        let neg = negotiate_exclusive(carrier, &self.driver.pcm_formats).map_err(|_| {
            LyraError::Device(format!(
                "DoP-capable ASIO driver '{}' does not accept 24-bit at {} Hz (DoP carrier for {:?})",
                self.driver.name,
                carrier.rate,
                dsd.rate
            ))
        })?;
        self.stream = Some(AsioStream::Pcm { source: carrier, neg });
        Ok(StreamSpec::Pcm(neg.spec))
    }
}

impl OutputDevice for AsioMockDevice {
    fn open(&mut self, spec: StreamSpec, mode: OutputMode) -> Result<StreamSpec> {
        let OutputMode::Asio { native_dsd } = mode else {
            return Err(LyraError::UnsupportedFormat(
                "AsioMockDevice only supports OutputMode::Asio".into(),
            ));
        };
        self.driver.captured.clear();
        match spec {
            StreamSpec::Pcm(pcm) => {
                if native_dsd {
                    return Err(LyraError::InvalidState(
                        "native_dsd requested for a PCM stream".into(),
                    ));
                }
                self.driver.set_io_format(AsioIoFormat::Pcm)?;
                let neg = negotiate_exclusive(pcm, &self.driver.pcm_formats)?;
                self.stream = Some(AsioStream::Pcm { source: pcm, neg });
                Ok(StreamSpec::Pcm(neg.spec))
            }
            StreamSpec::Dsd(dsd) => self.open_dsd(dsd, native_dsd),
        }
    }

    fn start(&mut self) -> Result<()> {
        if self.stream.is_none() {
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
        let stream = self
            .stream
            .ok_or_else(|| LyraError::InvalidState("write before open".into()))?;
        match stream {
            AsioStream::NativeDsd { spec } => {
                // Native DSD: pass the raw DSD bytes straight through, no
                // conversion. The input must already be a DSD stream with the
                // negotiated channel count.
                let StreamSpec::Dsd(in_spec) = frames.spec else {
                    return Err(LyraError::UnsupportedFormat(
                        "native DSD stream expects DSD frames".into(),
                    ));
                };
                if in_spec.channels != spec.channels {
                    return Err(LyraError::InvalidState(
                        "native DSD channel count differs from the opened stream".into(),
                    ));
                }
                self.driver.buffer_switch(frames.data);
                Ok(frames.frames)
            }
            AsioStream::Pcm { source, neg } => {
                // Plain PCM or the DoP carrier: apply the container padding so the
                // captured bytes are exactly what the driver buffer would hold.
                // For a packed 24-in-3 DoP carrier the padding is a no-op, so the
                // DoP marker bytes pass through untouched.
                let StreamSpec::Pcm(p) = frames.spec else {
                    return Err(LyraError::UnsupportedFormat(
                        "PCM stream expects PCM frames".into(),
                    ));
                };
                debug_assert_eq!(p.bytes_per_frame(), source.bytes_per_frame());
                let in_bytes = &frames.data[..frames.frames * source.bytes_per_frame()];
                pad_frames(source, neg.spec.format, in_bytes, &mut self.scratch);
                self.driver.buffer_switch(&self.scratch);
                Ok(frames.frames)
            }
        }
    }

    fn caps(&self) -> DeviceCaps {
        DeviceCaps {
            name: self.driver.name.clone(),
            // Reached through an ASIO driver, not WASAPI: exclusive/shared are the
            // WASAPI modes, so both are false here.
            exclusive: false,
            shared: false,
            asio: true,
            dop: self.driver.dop_capable,
            native_dsd: !self.driver.dsd_rates.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dsd(rate: DsdRate, msb_first: bool) -> DsdSpec {
        DsdSpec { rate, channels: 2, msb_first }
    }

    fn dsd_frames(spec: DsdSpec, data: &[u8]) -> Frames<'_> {
        // One DSD "frame" here = one byte per channel (8 one-bit samples/ch).
        Frames { spec: StreamSpec::Dsd(spec), data, frames: data.len() / spec.channels as usize }
    }

    // --- caps -------------------------------------------------------------

    #[test]
    fn caps_pcm_only() {
        let d = AsioMockDevice::new(AsioMockDriver::pcm_only("mock pcm"));
        let c = d.caps();
        assert!(c.asio);
        assert!(!c.dop);
        assert!(!c.native_dsd);
        assert!(!c.exclusive);
    }

    #[test]
    fn caps_dop_capable() {
        let d = AsioMockDevice::new(AsioMockDriver::pcm_dop("mock dop"));
        let c = d.caps();
        assert!(c.asio);
        assert!(c.dop);
        assert!(!c.native_dsd);
    }

    #[test]
    fn caps_native_dsd() {
        let d = AsioMockDevice::new(AsioMockDriver::native_dsd(
            "mock dsd",
            vec![DsdRate::Dsd64, DsdRate::Dsd128],
            true,
        ));
        let c = d.caps();
        assert!(c.asio);
        assert!(c.native_dsd);
        assert!(!c.dop); // native-DSD device here is not separately DoP-flagged
    }

    // --- native DSD passthrough ------------------------------------------

    #[test]
    fn native_dsd_passthrough_is_bit_exact() {
        let spec = dsd(DsdRate::Dsd64, true);
        let mut dev = AsioMockDevice::new(AsioMockDriver::native_dsd(
            "mock dsd",
            vec![DsdRate::Dsd64],
            true,
        ));
        let neg = dev.open(StreamSpec::Dsd(spec), OutputMode::Asio { native_dsd: true }).unwrap();
        assert!(matches!(neg, StreamSpec::Dsd(_)));
        assert_eq!(dev.driver.io_format(), AsioIoFormat::Dsd);
        dev.start().unwrap();

        let src: Vec<u8> = (0u16..512).map(|i| (i * 7 % 256) as u8).collect();
        let f = dsd_frames(spec, &src);
        dev.write(&f).unwrap();
        assert_eq!(dev.driver.captured, src, "DSD bytes must pass through unchanged");
    }

    #[test]
    fn native_dsd_bit_exact_across_chunk_sizes() {
        let spec = dsd(DsdRate::Dsd128, true);
        let src: Vec<u8> = (0u32..4096).map(|i| (i.wrapping_mul(31) % 251) as u8).collect();
        for chunk in [1usize, 2, 3, 7, 64, 333, 1024] {
            let mut dev = AsioMockDevice::new(AsioMockDriver::native_dsd(
                "mock dsd",
                vec![DsdRate::Dsd128],
                true,
            ));
            dev.open(StreamSpec::Dsd(spec), OutputMode::Asio { native_dsd: true }).unwrap();
            dev.start().unwrap();
            // Keep each write a whole number of frames (channels bytes).
            let step = (chunk / spec.channels as usize).max(1) * spec.channels as usize;
            for part in src.chunks(step) {
                dev.write(&dsd_frames(spec, part)).unwrap();
            }
            assert_eq!(dev.driver.captured, src, "chunk size {chunk} mangled the stream");
        }
    }

    #[test]
    fn native_dsd_reports_driver_bit_order() {
        // Source is LSB-first (DSF); driver wants MSB-first. The negotiated spec
        // reflects the driver's order — reconciliation is the decoder's job.
        let spec = dsd(DsdRate::Dsd64, false);
        let mut dev = AsioMockDevice::new(AsioMockDriver::native_dsd(
            "mock dsd msb",
            vec![DsdRate::Dsd64],
            true,
        ));
        let neg = dev.open(StreamSpec::Dsd(spec), OutputMode::Asio { native_dsd: true }).unwrap();
        match neg {
            StreamSpec::Dsd(d) => assert!(d.msb_first),
            StreamSpec::Pcm(_) => panic!("expected DSD"),
        }
    }

    // --- DoP over ASIO ----------------------------------------------------

    #[test]
    fn dop_carrier_rate_and_container() {
        let spec = dsd(DsdRate::Dsd64, true);
        let mut dev = AsioMockDevice::new(AsioMockDriver::pcm_dop("mock dop"));
        let neg = dev.open(StreamSpec::Dsd(spec), OutputMode::Asio { native_dsd: false }).unwrap();
        match neg {
            StreamSpec::Pcm(p) => {
                assert_eq!(p.rate, 176_400); // DSD64 / 16
                assert_eq!(p.bits_valid, 24);
                // Packed 24-in-3 is preferred, so the DoP word is carried verbatim.
                assert_eq!(p.format, SampleFormat::S24);
            }
            StreamSpec::Dsd(_) => panic!("DoP must negotiate a PCM carrier"),
        }
        assert_eq!(dev.driver.io_format(), AsioIoFormat::Pcm);
    }

    #[test]
    fn dop_packed_buffer_passes_through_markers_untouched() {
        let spec = dsd(DsdRate::Dsd64, true);
        let mut dev = AsioMockDevice::new(AsioMockDriver::pcm_dop("mock dop"));
        let carrier = dev
            .open(StreamSpec::Dsd(spec), OutputMode::Asio { native_dsd: false })
            .unwrap();
        let StreamSpec::Pcm(cspec) = carrier else { panic!("expected PCM carrier") };
        dev.start().unwrap();

        // Stand-in DoP buffer (DopPacker lives in lyra-dsd; this is the seam):
        // two stereo frames of packed 24-bit words, top byte = DoP marker
        // 0x05 then 0xFA, each word LE [data_lo, data_hi, marker].
        let dop: Vec<u8> = vec![
            0xAA, 0xBB, 0x05, 0xCC, 0xDD, 0x05, // frame 0 (L, R), marker 0x05
            0x11, 0x22, 0xFA, 0x33, 0x44, 0xFA, // frame 1 (L, R), marker 0xFA
        ];
        let f = Frames { spec: StreamSpec::Pcm(cspec), data: &dop, frames: 2 };
        dev.write(&f).unwrap();
        // Packed S24 carrier ⇒ no padding ⇒ captured equals the DoP buffer exactly,
        // marker bytes included.
        assert_eq!(dev.driver.captured, dop);
    }

    // --- error paths ------------------------------------------------------

    #[test]
    fn dsd_to_plain_pcm_device_errors() {
        let spec = dsd(DsdRate::Dsd64, true);
        let mut dev = AsioMockDevice::new(AsioMockDriver::pcm_only("mock pcm"));
        let err = dev
            .open(StreamSpec::Dsd(spec), OutputMode::Asio { native_dsd: false })
            .unwrap_err();
        assert!(matches!(err, LyraError::UnsupportedFormat(_)));
    }

    #[test]
    fn native_dsd_unsupported_rate_errors() {
        let spec = dsd(DsdRate::Dsd256, true);
        let mut dev = AsioMockDevice::new(AsioMockDriver::native_dsd(
            "mock dsd",
            vec![DsdRate::Dsd64], // no 256
            true,
        ));
        let err = dev
            .open(StreamSpec::Dsd(spec), OutputMode::Asio { native_dsd: true })
            .unwrap_err();
        assert!(matches!(err, LyraError::UnsupportedFormat(_)));
    }

    #[test]
    fn native_dsd_on_dop_only_device_errors() {
        // DoP-capable but no kASIODSDFormat: a native-DSD request must fail, not
        // silently fall back to DoP.
        let spec = dsd(DsdRate::Dsd64, true);
        let mut dev = AsioMockDevice::new(AsioMockDriver::pcm_dop("mock dop"));
        let err = dev
            .open(StreamSpec::Dsd(spec), OutputMode::Asio { native_dsd: true })
            .unwrap_err();
        assert!(matches!(err, LyraError::UnsupportedFormat(_)));
    }

    // --- plain PCM over ASIO ---------------------------------------------

    #[test]
    fn plain_pcm_over_asio_negotiates_and_captures() {
        let mut dev = AsioMockDevice::new(AsioMockDriver::pcm_only("mock pcm"));
        let src = PcmSpec { rate: 96_000, channels: 2, bits_valid: 24, format: SampleFormat::S24 };
        let neg = dev.open(StreamSpec::Pcm(src), OutputMode::Asio { native_dsd: false }).unwrap();
        match neg {
            StreamSpec::Pcm(p) => assert_eq!(p.format, SampleFormat::S24),
            StreamSpec::Dsd(_) => panic!("expected PCM"),
        }
        dev.start().unwrap();
        let data = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66]; // one stereo 24-bit frame
        let f = Frames { spec: StreamSpec::Pcm(src), data: &data, frames: 1 };
        dev.write(&f).unwrap();
        assert_eq!(dev.driver.captured, data);
    }

    #[test]
    fn write_before_start_errors() {
        let mut dev = AsioMockDevice::new(AsioMockDriver::pcm_only("mock pcm"));
        let src = PcmSpec { rate: 48_000, channels: 2, bits_valid: 24, format: SampleFormat::S24 };
        dev.open(StreamSpec::Pcm(src), OutputMode::Asio { native_dsd: false }).unwrap();
        let data = [0u8; 6];
        let f = Frames { spec: StreamSpec::Pcm(src), data: &data, frames: 1 };
        assert!(matches!(dev.write(&f), Err(LyraError::InvalidState(_))));
    }

    #[test]
    fn native_dsd_requested_for_pcm_errors() {
        let mut dev = AsioMockDevice::new(AsioMockDriver::native_dsd(
            "mock dsd",
            vec![DsdRate::Dsd64],
            true,
        ));
        let src = PcmSpec { rate: 48_000, channels: 2, bits_valid: 24, format: SampleFormat::S24 };
        let err = dev
            .open(StreamSpec::Pcm(src), OutputMode::Asio { native_dsd: true })
            .unwrap_err();
        assert!(matches!(err, LyraError::InvalidState(_)));
    }
}
