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

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lyra_core::{LyraError, OutputMode, PcmSpec, Result, SampleFormat, StreamSpec};
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

    /// Discard every unplayed byte still buffered toward the device so the next
    /// bytes pushed are the next thing it plays. The worker calls this on
    /// seek/stop/set-queue (never on pause) to drop the stale pre-seek tail that
    /// would otherwise play before the landed audio.
    ///
    /// Runs on the decode/worker thread and may block briefly (the real backend
    /// hands off to its feeder thread and waits for the ring to drain); it must
    /// never be called on a real-time render thread. Discarded frames were never
    /// played, so `frames_played` does not rewind.
    fn flush(&mut self);

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

/// Build an **offline** sink factory: no audio device is opened; every device-format
/// byte the worker would push is written to `path` (continuously across gapless
/// joins) and consumed instantly. This is the headless, silent bit-perfect capture
/// path for QA — the bytes are exactly what a real device tap would record, so the
/// file compares against the golden decode directly (ADR-004).
pub(crate) fn offline_sink_factory(path: PathBuf) -> Result<SinkFactory> {
    let file = File::create(&path)
        .map_err(|e| LyraError::Io(format!("creating offline tap {}: {e}", path.display())))?;
    let writer = Arc::new(Mutex::new(BufWriter::new(file)));
    Ok(Box::new(move |spec: &StreamSpec| -> Result<Box<dyn AudioSink>> {
        let pcm = match spec {
            StreamSpec::Pcm(p) => *p,
            StreamSpec::Dsd(_) => {
                return Err(LyraError::Device(
                    "offline sink carries PCM/DoP frames, not a raw DSD spec".into(),
                ))
            }
        };
        Ok(Box::new(OfflineSink {
            spec: *spec,
            pcm,
            writer: writer.clone(),
            played: Arc::new(AtomicU64::new(0)),
        }))
    }))
}

/// An [`AudioSink`] that writes device bytes to a shared file instead of a device.
struct OfflineSink {
    spec: StreamSpec,
    pcm: PcmSpec,
    writer: Arc<Mutex<BufWriter<File>>>,
    played: Arc<AtomicU64>,
}

impl AudioSink for OfflineSink {
    fn negotiated(&self) -> StreamSpec {
        // No container widening offline: the worker pads source → this same spec
        // (identity), so the file holds source-format bytes for a direct compare.
        self.spec
    }

    fn is_bit_perfect(&self) -> bool {
        // The offline file records bytes unaltered; whether the *source* path is
        // bit-perfect is decided by the delivery (DSD→PCM sets `processed`).
        !matches!(self.pcm.format, SampleFormat::F32)
    }

    fn push(&mut self, bytes: &[u8]) -> usize {
        let bpf = self.pcm.bytes_per_frame().max(1);
        let whole = (bytes.len() / bpf) * bpf;
        if whole > 0 {
            let mut w = self.writer.lock().expect("offline writer");
            if w.write_all(&bytes[..whole]).is_err() {
                return 0;
            }
            self.played.fetch_add((whole / bpf) as u64, Ordering::Relaxed);
        }
        whole
    }

    fn flush(&mut self) {
        // No-op: the offline sink consumes every pushed byte instantly and does
        // not model the ring/device boundary, so an offline capture of a *seek*
        // still contains the pre-seek region. Offline is the continuous-playback
        // byte-equality proof (ADR-004), not a seek-realism proof — the stale
        // real-device tail is exactly what `flush` drops on hardware.
    }

    fn frames_played(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }

    fn underruns(&self) -> u64 {
        0
    }

    fn device_lost(&self) -> bool {
        false
    }
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

        fn flush(&mut self) {
            // Discard the ring via the RT-safe producer→feeder handshake. Blocks
            // on this (worker) thread until drained or the backend's timeout.
            self.handle.flush();
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
