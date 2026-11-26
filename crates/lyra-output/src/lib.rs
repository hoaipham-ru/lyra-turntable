//! S04 — output: bit-perfect WASAPI exclusive/shared render with a device tap.
//!
//! This crate opens an audio endpoint **at the source rate and a container that
//! losslessly holds the source bits** (never resampling, never dropping bits —
//! ADR-004) and feeds it from a lock-free ring buffer on a real-time thread.
//!
//! # Layout
//! - [`negotiate`] — pure exclusive-mode format negotiation (ADR-004 / ADR-010).
//! - [`pad`] — zero-LSB container padding (16→24/32 etc.), run off the RT thread.
//! - [`ring`] — the RT feeder's pull/underrun accounting (pure, unit-tested).
//! - [`stats`] — atomic playback counters (the only state the RT thread writes).
//! - [`tap`] — the device tap: a second lock-free ring copied to a file off-thread.
//! - [`mock`] — an in-process fake [`lyra_core::OutputDevice`] for the WASAPI
//!   paths (format negotiation, padding) in tests.
//! - [`asio_mock`] — an in-process fake ASIO driver + [`lyra_core::OutputDevice`]
//!   for the ASIO paths (native DSD, DoP-over-ASIO, plain PCM) in tests (P5.4).
//! - `wasapi` (Windows only) — the real backend: enumeration, `IsFormatSupported`
//!   probing, the event-driven exclusive stream with push-mode fallback, MMCSS
//!   "Pro Audio", and the exclusive capture helper.
//! - `asio` (Windows + `asio` feature + ASIO SDK only) — the real ASIO backend
//!   via `asio-sys`. **Compiled out by default** (no SDK on this machine, ADR-009);
//!   its behaviour is proven structurally plus through [`asio_mock`], and on real
//!   hardware behind the pending-hardware human gate.
//!
//! # Real-time rules (PLAN §5, enforced by review)
//! The feeder thread does **no allocation, no locking, no logging, no file I/O**
//! once it is running; it only reads a pre-sized ring with lock-free chunk reads,
//! copies into a pre-allocated scratch buffer, hands bytes to the device API, and
//! updates [`stats::SharedStats`] with relaxed atomics. The tap and the decode
//! side run on ordinary threads.

#![cfg_attr(not(windows), allow(dead_code))]

pub mod asio_mock;
pub mod mock;
pub mod negotiate;
pub mod pad;
pub mod ring;
pub mod stats;
pub mod tap;
pub mod types;

pub use negotiate::{negotiate_exclusive, ExclusiveFormat, Negotiated, PROBE_CANDIDATES};
pub use pad::pad_frames;
pub use stats::{OutputStats, SharedStats};
pub use types::{DeviceId, DeviceInfo, DeviceProbe, OutputOptions, PROBE_RATES};

// Real ASIO backend: compiled only with the `asio` feature AND the ASIO SDK
// present (via `CPAL_ASIO_DIR`). Off by default so the SDK-less workspace build
// stays green (ADR-002 / ADR-009). See the module docs.
#[cfg(all(windows, feature = "asio"))]
mod asio_backend;

#[cfg(windows)]
mod handle;
#[cfg(windows)]
mod mmcss;
#[cfg(windows)]
mod wasapi_backend;

#[cfg(windows)]
pub use handle::OutputHandle;
#[cfg(windows)]
pub use wasapi_backend::{capture_exclusive, devices, open, probe, CaptureConfig};

// --- Non-Windows stubs -----------------------------------------------------
// The output layer is Windows-only by design (ADR-002: WASAPI exclusive / ASIO).
// These stubs keep `cargo build`/`cargo check` green on other hosts so the pure
// logic above can still be developed and tested cross-platform.
#[cfg(not(windows))]
mod not_windows {
    use lyra_core::{LyraError, OutputMode, Result, StreamSpec};

    use crate::types::{DeviceId, DeviceInfo, DeviceProbe, OutputOptions};

    fn unsupported() -> LyraError {
        LyraError::Device("WASAPI output is only available on Windows (ADR-002)".into())
    }

    /// See the Windows implementation. Returns an error off-Windows.
    pub fn devices() -> Result<Vec<DeviceInfo>> {
        Err(unsupported())
    }

    /// See the Windows implementation. Returns an error off-Windows.
    pub fn probe(_dev: &DeviceId) -> Result<DeviceProbe> {
        Err(unsupported())
    }

    /// Opaque handle placeholder off-Windows; [`open`] never returns one.
    pub struct OutputHandle;

    /// See the Windows implementation. Returns an error off-Windows.
    pub fn open(
        _dev: &DeviceId,
        _spec: &StreamSpec,
        _mode: OutputMode,
        _opts: OutputOptions,
    ) -> Result<OutputHandle> {
        Err(unsupported())
    }
}

#[cfg(not(windows))]
pub use not_windows::{devices, open, probe, OutputHandle};
