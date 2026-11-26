//! The output-sink abstraction.
//!
//! The decode worker feeds the device through this trait rather than through a
//! concrete [`lyra_output::OutputHandle`]. That keeps the worker's transport logic
//! testable against an in-memory fake (no audio, no hardware) and keeps the engine
//! compiling off-Windows, where the real WASAPI path is only a stub.
//!
//! The engine only *moves bytes* through a sink — it never reads or alters sample
//! values. Padding into the device container happens before `push` (off the
//! real-time render thread), exactly as `lyra-play` does it.

use lyra_core::{OutputMode, Result, StreamSpec};
use lyra_output::{DeviceId, OutputOptions};

/// A live output stream the worker pushes device-format bytes into.
///
/// Implementations are `Send` so the worker thread can own one. `push` is
/// non-blocking: it returns how many of the given bytes the ring accepted, and the
/// caller carries the remainder to the next tick (back-pressure without blocking
/// commands). The counters come straight from [`lyra_output::OutputStats`].
pub(crate) trait AudioSink: Send {
    /// The format the device was opened with (its container may be wider than the
    /// source; padding bridges the gap).
    fn negotiated(&self) -> StreamSpec;

    /// `true` when this path does not alter samples (exclusive mode, or shared mode
    /// whose mix already matches the source).
    fn is_bit_perfect(&self) -> bool;

    /// Push device-format bytes into the ring, returning how many were accepted.
    fn push(&mut self, bytes: &[u8]) -> usize;

    /// Frames the device has actually consumed since the stream opened.
    fn frames_played(&self) -> u64;

    /// Cumulative underrun events.
    fn underruns(&self) -> u64;

    /// `true` once the device was invalidated / unplugged mid-stream.
    fn device_lost(&self) -> bool;
}

/// Opens an [`AudioSink`] for a requested device [`StreamSpec`]. Boxed so tests can
/// inject a fake factory and the real one captures the device/mode/options.
pub(crate) type SinkFactory = Box<dyn FnMut(&StreamSpec) -> Result<Box<dyn AudioSink>> + Send>;

/// Build the real WASAPI-backed sink factory from the engine configuration.
pub(crate) fn wasapi_sink_factory(
    device: DeviceId,
    mode: OutputMode,
    opts: OutputOptions,
) -> SinkFactory {
    Box::new(move |spec: &StreamSpec| real::open(&device, spec, mode, opts.clone()))
}

#[cfg(windows)]
mod real {
    use super::AudioSink;
    use lyra_core::{OutputMode, Result, StreamSpec};
    use lyra_output::{DeviceId, OutputHandle, OutputOptions};

    /// Wraps a live [`OutputHandle`]. Dropping it stops the render/tap threads and
    /// releases the device (handled by `OutputHandle`'s own `Drop`).
    struct WasapiSink {
        handle: OutputHandle,
    }

    impl AudioSink for WasapiSink {
        fn negotiated(&self) -> StreamSpec {
            self.handle.negotiated()
        }

        fn is_bit_perfect(&self) -> bool {
            self.handle.is_bit_perfect()
        }

        fn push(&mut self, bytes: &[u8]) -> usize {
            let (_written, remainder) = self.handle.producer().push_partial_slice(bytes);
            bytes.len() - remainder.len()
        }

        fn frames_played(&self) -> u64 {
            self.handle.stats().frames_played
        }

        fn underruns(&self) -> u64 {
            self.handle.stats().underruns
        }

        fn device_lost(&self) -> bool {
            self.handle.stats().device_lost
        }
    }

    pub(super) fn open(
        device: &DeviceId,
        spec: &StreamSpec,
        mode: OutputMode,
        opts: OutputOptions,
    ) -> Result<Box<dyn AudioSink>> {
        let handle = lyra_output::open(device, spec, mode, opts)?;
        Ok(Box::new(WasapiSink { handle }))
    }
}

#[cfg(not(windows))]
mod real {
    use super::AudioSink;
    use lyra_core::{LyraError, OutputMode, Result, StreamSpec};
    use lyra_output::{DeviceId, OutputOptions};

    pub(super) fn open(
        _device: &DeviceId,
        _spec: &StreamSpec,
        _mode: OutputMode,
        _opts: OutputOptions,
    ) -> Result<Box<dyn AudioSink>> {
        Err(LyraError::Device(
            "WASAPI output is only available on Windows (ADR-002)".into(),
        ))
    }
}
