//! The **real ASIO backend** (Windows + `asio` feature + ASIO SDK), via
//! `asio-sys` (ADR-002). Scaffolding only in this task (P5.4).
//!
//! # Why this is compiled out by default
//! `asio-sys` needs the Steinberg ASIO SDK at build time, located through the
//! `CPAL_ASIO_DIR` environment variable (ADR-002). **The SDK is never committed**
//! and is not present on the dev laptop (ADR-009), so this module is gated behind
//! the `asio` Cargo feature *and* `#[cfg(windows)]`:
//!
//! ```text
//! #[cfg(all(windows, feature = "asio"))]
//! ```
//!
//! The default `cargo build` / `cargo test --workspace` never enables `asio`, so
//! this file is not compiled and no SDK is required. Build the real backend with:
//!
//! ```text
//! $env:CPAL_ASIO_DIR = "C:\path\to\asiosdk"
//! cargo build -p lyra-output --features asio
//! ```
//!
//! # Verification status (ADR-009)
//! The behaviour below is proven two ways *without* hardware: the pure negotiation
//! / padding / ring logic is shared with the WASAPI path and unit-tested, and the
//! native-DSD / DoP / PCM decision logic is unit-tested against
//! [`crate::asio_mock::AsioMockDevice`]. Running it against a **real** ASIO driver
//! is a pending-hardware human gate that stays open until a DSD-capable DAC with an
//! ASIO driver is connected; 1.0 may ship with that gate marked "pending hardware".
//!
//! # Real-time rules (PLAN §5)
//! ASIO calls back on its own high-priority driver thread (`bufferSwitch`). That
//! callback does **no allocation, no locking, no logging, no file I/O**: it pulls
//! from the pre-sized ring with [`crate::ring::pull_into`] (a lock-free memcpy),
//! copies into the driver's buffer, optionally copies into the tap ring
//! ([`crate::tap::TapWriter::push`]), and bumps relaxed atomics
//! ([`crate::stats::SharedStats`]) — identical discipline to the WASAPI feeder.
//! Pre-fill: PCM silence via [`crate::ring::silence`]; native DSD and DoP use the
//! DoP-valid idle pattern ([`crate::ring::dop_idle`]) rather than PCM zeros.

#![cfg(all(windows, feature = "asio"))]

// NOTE: scaffolding. The concrete `asio-sys` FFI calls (`asio_sys::ASIOInit`,
// `ASIOGetChannels`, `ASIOCanSampleRate`, `ASIOSetIoFormat` with `kASIODSDFormat`,
// `ASIOCreateBuffers`, `ASIOStart`/`ASIOStop`) are wired here when the SDK is
// available and exercised on the pending-hardware gate. The decision logic
// (native DSD vs DoP vs reject) mirrors `crate::asio_mock::AsioMockDevice`, which
// is the executable specification for this module.

use lyra_core::{DsdSpec, OutputMode, PcmSpec, Result, StreamSpec};

use crate::asio_mock::AsioIoFormat;

/// A discovered ASIO driver (name + index as `asio-sys` enumerates them).
#[derive(Debug, Clone)]
pub struct AsioDriverInfo {
    /// Driver name as shown by `asio-sys`/the control panel.
    pub name: String,
    /// Driver index within the host's driver list.
    pub index: usize,
}

/// Enumerate installed ASIO drivers (`asio-sys` driver list). Empty when no ASIO
/// driver is installed (the dev laptop, ADR-009).
pub fn drivers() -> Result<Vec<AsioDriverInfo>> {
    // TODO(pending-hardware): `asio_sys::get_driver_list()` → map to AsioDriverInfo.
    unimplemented!("real ASIO enumeration — pending-hardware gate (ADR-009)")
}

/// Probe which sample rates / formats a driver accepts, including `kASIODSDFormat`
/// at each DSD rate (`ASIOCanSampleRate` after `ASIOSetIoFormat(kASIODSDFormat)`).
/// Feeds the capability report `lyra-play --probe` prints (ADR-009).
pub fn probe(_driver: &AsioDriverInfo) -> Result<()> {
    // TODO(pending-hardware): ASIOInit → ASIOGetChannels → for each candidate PCM
    // rate call ASIOCanSampleRate; set kASIODSDFormat and repeat for DSD rates.
    unimplemented!("real ASIO probe — pending-hardware gate (ADR-009)")
}

/// Decide the ASIO I/O format and carrier for `spec` + `mode`, mirroring
/// [`crate::asio_mock::AsioMockDevice`]'s rules so the mock stays the executable
/// spec for the real path (native DSD → DoP → reject; DSD→PCM is lyra-dsd).
pub fn plan_stream(spec: StreamSpec, mode: OutputMode) -> Result<AsioIoFormat> {
    match (spec, mode) {
        (StreamSpec::Dsd(_), OutputMode::Asio { native_dsd: true }) => Ok(AsioIoFormat::Dsd),
        (StreamSpec::Dsd(_) | StreamSpec::Pcm(_), OutputMode::Asio { .. }) => Ok(AsioIoFormat::Pcm),
        _ => Err(lyra_core::LyraError::UnsupportedFormat(
            "ASIO backend only serves OutputMode::Asio".into(),
        )),
    }
}

/// Open an ASIO stream for `spec` in `mode`. Spawns the driver, negotiates the
/// format (reusing [`crate::negotiate`] for PCM / DoP carriers), creates the ASIO
/// buffers and installs the `bufferSwitch` callback that drains the ring.
pub fn open(
    _driver: &AsioDriverInfo,
    _spec: StreamSpec,
    _mode: OutputMode,
) -> Result<crate::OutputHandle> {
    // TODO(pending-hardware): COM init, ASIOInit, ASIOSetIoFormat (PCM or
    // kASIODSDFormat), negotiate container via crate::negotiate for PCM/DoP,
    // ASIOCreateBuffers(buffer_frames), register bufferSwitch → ring::pull_into,
    // MMCSS "Pro Audio" is driver-managed for the callback thread.
    unimplemented!("real ASIO open — pending-hardware gate (ADR-009)")
}

/// The DoP carrier PCM format for a DSD stream — identical to the mock's rule, so
/// the real and mock paths negotiate the same container.
#[must_use]
pub fn dop_carrier_spec(dsd: DsdSpec) -> PcmSpec {
    crate::asio_mock::AsioMockDevice::dop_carrier_spec(dsd)
}
