//! [`OutputHandle`]: the owner-side handle to a running WASAPI stream.
//!
//! The render (feeder) thread owns the device and the ring consumer; this handle
//! owns the ring **producer** the caller pushes device-format bytes into, a
//! snapshot of the negotiated format, the shared atomic counters, and the join
//! handles for the feeder and the optional tap-drain thread. Dropping or
//! [`OutputHandle::stop`]-ping it tears the stream down in the right order.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use lyra_core::StreamSpec;

use crate::stats::{OutputStats, SharedStats};

/// A live output stream. Push device-format bytes into [`OutputHandle::producer`];
/// the feeder thread drains them to the device.
pub struct OutputHandle {
    pub(crate) producer: rtrb::Producer<u8>,
    pub(crate) negotiated: StreamSpec,
    pub(crate) bit_perfect: bool,
    pub(crate) stats: Arc<SharedStats>,
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) feeder: Option<JoinHandle<()>>,
    pub(crate) tap_drain: Option<JoinHandle<io::Result<()>>>,
    pub(crate) tap_dropped: Option<Arc<AtomicU64>>,
}

impl OutputHandle {
    /// The ring producer. Push bytes **in the negotiated device format** here
    /// (pad the source with [`crate::pad_frames`] first, off the render thread).
    pub fn producer(&mut self) -> &mut rtrb::Producer<u8> {
        &mut self.producer
    }

    /// The format the device was actually opened with (may be a wider container
    /// than the source; see [`crate::Negotiated`]).
    #[must_use]
    pub fn negotiated(&self) -> StreamSpec {
        self.negotiated
    }

    /// `true` when the path does not alter samples (exclusive mode, or shared mode
    /// whose mix format already matches the source).
    #[must_use]
    pub fn is_bit_perfect(&self) -> bool {
        self.bit_perfect
    }

    /// A snapshot of the playback counters.
    #[must_use]
    pub fn stats(&self) -> OutputStats {
        self.stats.snapshot(self.bit_perfect)
    }

    /// Bytes the tap had to drop (ring overflow). `None` if no tap was attached;
    /// `Some(0)` proves the tap captured the whole stream.
    #[must_use]
    pub fn tap_dropped(&self) -> Option<u64> {
        self.tap_dropped.as_ref().map(|d| d.load(Ordering::Relaxed))
    }

    /// Stop the stream and wait for the feeder and tap-drain threads to finish.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Signal the feeder to stop, drop the producer so the ring is abandoned, and
    /// join both threads. Idempotent.
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(feeder) = self.feeder.take() {
            let _ = feeder.join();
        }
        // The feeder drops its TapWriter on exit, abandoning the tap ring, so the
        // drain thread finishes on its own.
        if let Some(drain) = self.tap_drain.take() {
            let _ = drain.join();
        }
    }
}

impl Drop for OutputHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
