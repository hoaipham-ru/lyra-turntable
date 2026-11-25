//! The output device trait and its mode/capability types (S04).

use crate::error::Result;
use crate::spec::{Frames, StreamSpec};

/// How an output stream is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputMode {
    /// WASAPI exclusive mode: bit-perfect, silences other apps.
    WasapiExclusive,
    /// WASAPI shared mode: mixed by Windows, not bit-perfect.
    WasapiShared,
    /// ASIO. `native_dsd` requests a raw DSD stream to the driver rather than
    /// PCM (or DoP-over-PCM).
    Asio {
        /// Request native 1-bit DSD transport to the driver.
        native_dsd: bool,
    },
}

/// What an output device can do, reported by [`OutputDevice::caps`].
///
/// Used by the engine to pick a DSD path (native / DoP / convert, per ADR-009)
/// and to decide whether a given [`StreamSpec`] can play bit-perfect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCaps {
    /// Human-readable device name.
    pub name: String,
    /// Supports WASAPI exclusive mode.
    pub exclusive: bool,
    /// Supports WASAPI shared mode.
    pub shared: bool,
    /// Reachable through an ASIO driver.
    pub asio: bool,
    /// Marked as able to receive DoP markers (never assumed for unknown
    /// devices, per ADR-009).
    pub dop: bool,
    /// Accepts native DSD over ASIO.
    pub native_dsd: bool,
}

/// A hardware (or virtual) audio output.
///
/// The engine opens the device with a desired [`StreamSpec`] and [`OutputMode`],
/// then feeds it [`Frames`] through a ring buffer filled from the decode thread.
/// Implementations live in `lyra-output` (S04); the real-time rules (no
/// allocation, locking or logging on the device callback) are enforced there, not
/// by this trait.
pub trait OutputDevice {
    /// Open the device for `spec` in `mode` and return the format actually
    /// negotiated (which may differ, e.g. a wider container).
    fn open(&mut self, spec: StreamSpec, mode: OutputMode) -> Result<StreamSpec>;

    /// Start streaming. Must be called after [`OutputDevice::open`].
    fn start(&mut self) -> Result<()>;

    /// Stop streaming. The device may be re-[`OutputDevice::start`]ed.
    fn stop(&mut self) -> Result<()>;

    /// Enqueue `frames` into the device ring buffer, returning how many frames
    /// were accepted (fewer than requested when the buffer is full).
    fn write(&mut self, frames: &Frames<'_>) -> Result<usize>;

    /// Report what this device supports.
    fn caps(&self) -> DeviceCaps;
}
