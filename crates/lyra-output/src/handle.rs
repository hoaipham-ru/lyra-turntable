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
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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

    /// Discard every **unplayed** byte currently buffered in the ring, keeping the
    /// device open, so that bytes pushed into [`OutputHandle::producer`] *after*
    /// this call returns are the next thing the device plays — modulo the few ms
    /// already committed to the device's own hardware buffer, which is unavoidable
    /// and acceptable. The engine calls this on `Seek`/`Stop`/`SetQueue` so stale
    /// pre-seek audio (up to ~2 s of ring) does not play before the landed
    /// position (reviewer finding B1).
    ///
    /// # Mechanism
    /// The producer cannot move the consumer's read pointer, so this requests a
    /// flush generation and blocks until the feeder (render) thread drains the ring
    /// and acknowledges it — a lock-free, allocation-free discard on that thread.
    /// After this returns the caller should rebase its clock and push post-seek
    /// data; [`OutputStats::frames_played`](crate::OutputStats::frames_played) is
    /// **not** rewound (discarded bytes were never written to the device), and the
    /// intentional post-flush silence is not counted as an underrun.
    ///
    /// # Threading
    /// Call this from the engine's decode/worker thread — **not** the render
    /// thread. It may sleep. It is best-effort: it returns promptly (without an
    /// error) if the device is lost or if the feeder does not acknowledge within a
    /// short timeout, so a dying device can never hang the caller.
    pub fn flush(&self) {
        // Request a new flush generation; the feeder services >= this value.
        let target = self.stats.flush_req.fetch_add(1, Ordering::AcqRel) + 1;
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if self.stats.flush_ack.load(Ordering::Acquire) >= target {
                return; // feeder drained the ring
            }
            if self.stats.device_lost.load(Ordering::Relaxed) {
                return; // best-effort: a dead device never acks
            }
            if Instant::now() >= deadline {
                return; // best-effort timeout; never hang the caller
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Alias for [`OutputHandle::flush`]: discard buffered audio without tearing the
    /// device down (used by the engine on `Stop`/`SetQueue`, which keep the device
    /// open before new data arrives).
    pub fn reset(&self) {
        self.flush();
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

#[cfg(test)]
mod tests {
    use super::*;
    use lyra_core::{PcmSpec, SampleFormat};

    /// Build a handle with no feeder/tap (feeder: None) around a shared `stats`,
    /// so the flush handshake can be driven by hand. Drop is a no-op join.
    fn handle_with(stats: Arc<SharedStats>) -> OutputHandle {
        let (producer, _consumer) = rtrb::RingBuffer::<u8>::new(16);
        OutputHandle {
            producer,
            negotiated: StreamSpec::Pcm(PcmSpec {
                rate: 48_000,
                channels: 2,
                bits_valid: 24,
                format: SampleFormat::S32,
            }),
            bit_perfect: true,
            stats,
            stop: Arc::new(AtomicBool::new(false)),
            feeder: None,
            tap_drain: None,
            tap_dropped: None,
        }
    }

    #[test]
    fn flush_returns_promptly_when_device_lost() {
        let stats = Arc::new(SharedStats::default());
        stats.device_lost.store(true, Ordering::Relaxed);
        let handle = handle_with(Arc::clone(&stats));
        let start = Instant::now();
        handle.flush(); // no feeder will ever ack; must not hang
        assert!(start.elapsed() < Duration::from_millis(200), "flush must not block on a lost device");
        // A request was still recorded (the engine's seek still proceeds).
        assert_eq!(stats.flush_req.load(Ordering::Acquire), 1);
    }

    #[test]
    fn flush_unblocks_when_feeder_acks() {
        let stats = Arc::new(SharedStats::default());
        let handle = handle_with(Arc::clone(&stats));
        // Stand-in feeder: wait for a request, then ack it.
        let feeder_stats = Arc::clone(&stats);
        let feeder = thread::spawn(move || loop {
            let req = feeder_stats.flush_req.load(Ordering::Acquire);
            if req > feeder_stats.flush_ack.load(Ordering::Acquire) {
                feeder_stats.flush_ack.store(req, Ordering::Release);
                return;
            }
            thread::sleep(Duration::from_millis(1));
        });
        handle.flush();
        feeder.join().unwrap();
        assert_eq!(stats.flush_ack.load(Ordering::Acquire), 1);
        // No device loss recorded on this path.
        assert!(!stats.device_lost.load(Ordering::Relaxed));
    }
}
