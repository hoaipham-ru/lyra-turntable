//! Playback counters.
//!
//! [`SharedStats`] is the only state the real-time render thread writes, and it
//! writes it with plain atomics — no locks, no allocation (S04 real-time rules).
//! [`OutputStats`] is the snapshot the rest of the app reads.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Atomic counters shared between the render thread (writer) and the owning
/// [`crate::WasapiDevice`] (reader). Only atomic stores happen on the render
/// thread.
#[derive(Debug, Default)]
pub struct SharedStats {
    /// Frames handed to the device since the stream started.
    pub frames_played: AtomicU64,
    /// Number of underrun events (a buffer filled partly or wholly with silence
    /// because the producer could not keep up).
    pub underruns: AtomicU64,
    /// Set once if the device was invalidated / removed mid-stream.
    pub device_lost: AtomicBool,
    /// Flush generation **requested** by the owner side ([`crate::OutputHandle::flush`]).
    /// The producer bumps this to ask the feeder to discard all unplayed bytes in
    /// the ring. Monotonically increasing; starts at 0.
    pub flush_req: AtomicU64,
    /// Flush generation **acknowledged** by the feeder thread once it has drained
    /// the ring. The owner side waits for `flush_ack >= flush_req` to know the
    /// discard is complete. Only the feeder thread writes this; starts at 0.
    pub flush_ack: AtomicU64,
}

impl SharedStats {
    /// Read a consistent-enough snapshot for reporting.
    #[must_use]
    pub fn snapshot(&self, bit_perfect: bool) -> OutputStats {
        OutputStats {
            frames_played: self.frames_played.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            device_lost: self.device_lost.load(Ordering::Relaxed),
            bit_perfect,
        }
    }
}

/// A point-in-time view of playback health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputStats {
    /// Frames handed to the device since the stream started.
    pub frames_played: u64,
    /// Underrun events so far.
    pub underruns: u64,
    /// `true` if the device was invalidated / unplugged mid-stream.
    pub device_lost: bool,
    /// `true` when the negotiated path does not modify samples (exclusive mode);
    /// `false` in shared mode when the mix format differs from the source.
    pub bit_perfect: bool,
}
