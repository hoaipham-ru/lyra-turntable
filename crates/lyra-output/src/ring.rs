//! The real-time feeder's pull + underrun accounting.
//!
//! This is the one piece of render-thread logic that is worth testing on its own,
//! so it lives here as a pure function over an [`rtrb::Consumer`] and a
//! caller-owned scratch buffer. It performs **no allocation and takes no locks**:
//! [`rtrb::Consumer::pop_partial_slice`] is a lock-free memcpy into an existing
//! slice, and the underrun tail is a `memset` of already-owned memory. That keeps
//! it legal to call from the MMCSS "Pro Audio" render thread (PLAN §5).
//!
//! The device buffer is filled in whole frames: `scratch.len()` is always a frame
//! multiple, and the producer only ever writes whole frames, so the ring never
//! splits a frame across a pull.

use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::Consumer;

/// Service a pending flush request from the feeder (render) thread.
///
/// This is the RT-safe half of the producer→feeder flush handshake (reviewer
/// finding B1): the owner side bumps `flush_req` via
/// [`crate::OutputHandle::flush`], and the feeder — which *owns* the ring
/// consumer — is the only thread allowed to move the read pointer, so it must do
/// the discard here.
///
/// If a new flush generation is pending (`flush_req > flush_ack`), drain the
/// consumer to empty by repeatedly popping into the caller-owned `scratch` and
/// throwing the bytes away, then publish the serviced generation into
/// `flush_ack`. Returns `true` when a flush was serviced this call, so the caller
/// can **suppress the underrun** for the buffer it is about to zero-fill (the gap
/// is an intentional discard, not the producer falling behind). Returns `false`
/// when nothing was pending (a cheap pair of atomic loads).
///
/// Real-time safe: no allocation, no locks, no logging, no syscalls.
/// [`Consumer::pop_partial_slice`] is a lock-free memcpy into existing memory, and
/// the only shared state touched is two atomics. Legal on the MMCSS "Pro Audio"
/// render thread (PLAN §5).
///
/// Discarded bytes were never handed to the device, so `frames_played` is
/// untouched here — only the device write path advances it.
#[must_use]
pub fn service_flush(
    consumer: &mut Consumer<u8>,
    scratch: &mut [u8],
    flush_req: &AtomicU64,
    flush_ack: &AtomicU64,
) -> bool {
    // Observe the requested generation first; only ack a value we have seen.
    let target = flush_req.load(Ordering::Acquire);
    if target <= flush_ack.load(Ordering::Acquire) {
        return false; // nothing pending
    }
    // Drain the ring to empty: pop into the throwaway scratch until it yields
    // nothing. Wait-free; no allocation.
    loop {
        let (popped, _remainder) = consumer.pop_partial_slice(scratch);
        if popped.is_empty() {
            break;
        }
    }
    // Publish: the owner side waits for flush_ack >= its target. Store exactly the
    // generation we observed (a later request bumps flush_req again and will be
    // serviced on a subsequent iteration).
    flush_ack.store(target, Ordering::Release);
    true
}

/// Result of one feeder pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pull {
    /// Bytes actually copied out of the ring (the rest of `scratch` is silence).
    pub bytes_from_ring: usize,
    /// `true` if the ring could not fill the whole device buffer, so the tail was
    /// zero-filled. The caller increments the underrun counter when this is set.
    pub underrun: bool,
}

/// Fill `scratch` for one device buffer: copy as much as the ring holds, then
/// zero-fill any remainder (a PCM underrun is silence, ADR / S04 "pre-fill
/// silence correctly").
///
/// Returns how many bytes came from the ring and whether an underrun happened.
/// Never allocates; never logs; never blocks.
pub fn pull_into(consumer: &mut Consumer<u8>, scratch: &mut [u8]) -> Pull {
    let want = scratch.len();
    let (filled, remainder) = consumer.pop_partial_slice(scratch);
    let got = filled.len();
    // Underrun: the producer fell behind. Emit silence for the gap so the device
    // keeps a steady clock instead of glitching.
    for b in remainder.iter_mut() {
        *b = 0;
    }
    Pull { bytes_from_ring: got, underrun: got < want }
}

/// Post-flush grace accounting for the feeder (an implementation extension of
/// ADR-013's principle that *deliberate post-flush silence is not an underrun*).
///
/// ADR-013 already suppresses the empty-ring silence rendered **during** a flush
/// drain (the `flushed` iteration). But a flush is a deliberate discontinuity
/// (seek / track-change / reload), and right after the drain the engine must
/// **re-prime** the ring — decode and refill to its cushion — before steady
/// playback resumes. On a real device the render clock keeps calling `GetBuffer`
/// and drains the still-empty ring throughout that reprime window, so the feeder
/// would flag an underrun on nearly every deliberate transition even though the
/// gap is intentional, not the producer starving mid-playback.
///
/// [`FlushGrace`] closes that window. A flush **arms** a bounded grace state; while
/// armed, empty-ring iterations still render silence (the device never starves)
/// but do **not** count as underruns. Grace exits the moment the reprime lands —
/// the ring level climbs back to `low_water` (one whole device buffer, so the next
/// pull is guaranteed clean) — or after a `cap` of feeder iterations, so a producer
/// that **never** reprimes after a flush still surfaces underruns instead of being
/// masked forever.
///
/// Crucially, grace is entered **only** by a flush. Steady playback that never
/// flushes keeps `remaining == 0`, so every mid-playback underrun is counted: the
/// perf path stays honest.
///
/// Real-time safe: plain integer state; the per-iteration decision is branches and
/// integer math over caller-owned fields. Legal on the MMCSS "Pro Audio" render
/// thread (PLAN §5).
#[derive(Debug)]
pub struct FlushGrace {
    /// Feeder iterations of grace still remaining; `0` means not in grace.
    remaining: u32,
    /// Safety cap: the maximum number of feeder iterations to stay in grace if the
    /// reprime never lands (bounds how long a genuine post-flush stall is masked).
    cap: u32,
    /// Ring level (bytes) that means the reprime has landed. Set to one whole
    /// device buffer, so reaching it guarantees the next pull fills completely.
    low_water: usize,
}

impl FlushGrace {
    /// `low_water`: ring bytes that signal the reprime landed (one device buffer).
    /// `cap`: max feeder iterations to stay in grace if the reprime never lands.
    #[must_use]
    pub fn new(low_water: usize, cap: u32) -> Self {
        Self { remaining: 0, cap, low_water }
    }

    /// `true` while a flush grace window is open. Test-only introspection.
    #[cfg(test)]
    #[must_use]
    pub fn in_grace(&self) -> bool {
        self.remaining > 0
    }

    /// Per-iteration underrun accounting for the feeder. Call once per device-buffer
    /// iteration, after [`service_flush`] and [`pull_into`].
    ///
    /// * `flushed` — a flush was serviced this iteration (a deliberate discontinuity).
    /// * `ring_level` — bytes readable in the ring this iteration (`Consumer::slots`),
    ///   sampled **after** the flush drain and **before** the pull, so it reflects how
    ///   far the producer has reprimed.
    /// * `underrun` — the pull zero-filled part of the device buffer.
    ///
    /// Returns `true` if the caller should count this as an underrun, `false` if it
    /// is deliberate post-discontinuity silence to suppress.
    #[must_use]
    pub fn count_underrun(&mut self, flushed: bool, ring_level: usize, underrun: bool) -> bool {
        if flushed {
            // Deliberate discontinuity: (re)arm the grace window. The buffer we
            // zero-fill this iteration is the intentional discard, never an underrun.
            self.remaining = self.cap;
            return false;
        }
        if self.remaining > 0 {
            if ring_level >= self.low_water {
                // Reprime landed: a whole device buffer is queued again, so honest
                // counting resumes immediately and a *later* stall is still caught.
                self.remaining = 0;
            } else {
                // Still waiting for the engine to refill after the flush. The silence
                // is deliberate; count down toward the safety cap so a producer that
                // never reprimes eventually surfaces underruns.
                self.remaining -= 1;
                return false;
            }
        }
        // Not in grace (steady playback) or the reprime just landed: count honestly.
        underrun
    }
}

/// Fill `scratch` entirely with PCM silence (zeros). Used to pre-fill the device
/// buffer before `Start` so the first event does not underrun.
///
/// (DoP needs a non-zero idle pattern instead — see [`dop_idle`] — because a run
/// of PCM zeros is *not* valid DoP: it has no `0x05`/`0xFA` marker, so a DoP DAC
/// would fall out of DSD and could emit full-scale noise.)
pub fn silence(scratch: &mut [u8]) {
    for b in scratch.iter_mut() {
        *b = 0;
    }
}

/// The DSD data byte that encodes silence (mid-level): a 0x69/0x96 alternation is
/// the canonical idle pattern of a sigma-delta DSD stream. We use `0x69` for both
/// payload bytes of each DoP word, which every DoP DAC treats as silence.
const DOP_SILENCE_DATA: u8 = 0x69;

/// Pre-fill `scratch` with a **valid DoP idle pattern** (not PCM zeros) so the
/// device clocks silence while still seeing DSD-over-PCM, before `Start` and on an
/// underrun of a DoP/DSD stream (S04 "pre-fill silence correctly … DoP-valid idle
/// pattern for DoP").
///
/// `scratch` carries 24-bit words as **24-in-32** little-endian frames (4 bytes
/// per sample): `[0x00, data_lo, data_hi, marker]`, marker alternating `0x05` /
/// `0xFA` **per PCM frame** (all channels of a frame share one marker, per the DoP
/// standard). `start_marker_05` sets the marker of the first frame so the caller
/// can keep the alternation continuous across buffers; the returned bool is the
/// marker state to pass next (`true` ⇒ the next frame starts at `0x05`).
///
/// Allocation- and lock-free: a straight write over caller-owned memory, so it is
/// legal on the render thread.
#[must_use]
pub fn dop_idle(scratch: &mut [u8], channels: usize, mut start_marker_05: bool) -> bool {
    debug_assert!(channels >= 1);
    let frame = 4 * channels; // 24-in-32 container
    for chunk in scratch.chunks_mut(frame) {
        let marker = if start_marker_05 { 0x05 } else { 0xFA };
        for sample in chunk.chunks_mut(4) {
            // 24-in-32 LE: low byte unused (0), then DSD data, then marker on top.
            sample[0] = 0x00;
            if sample.len() > 1 {
                sample[1] = DOP_SILENCE_DATA;
            }
            if sample.len() > 2 {
                sample[2] = DOP_SILENCE_DATA;
            }
            if sample.len() > 3 {
                sample[3] = marker;
            }
        }
        // Only flip once a whole frame is emitted (a partial tail keeps the marker
        // so the next full frame continues the alternation correctly).
        if chunk.len() == frame {
            start_marker_05 = !start_marker_05;
        }
    }
    start_marker_05
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    #[test]
    fn full_pull_no_underrun() {
        let (mut p, mut c) = RingBuffer::<u8>::new(16);
        p.push_entire_slice(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let mut scratch = [0u8; 8];
        let r = pull_into(&mut c, &mut scratch);
        assert_eq!(r.bytes_from_ring, 8);
        assert!(!r.underrun);
        assert_eq!(scratch, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn partial_pull_zero_fills_and_flags_underrun() {
        let (mut p, mut c) = RingBuffer::<u8>::new(16);
        p.push_entire_slice(&[9, 9, 9, 9]).unwrap();
        let mut scratch = [0xAAu8; 8];
        let r = pull_into(&mut c, &mut scratch);
        assert_eq!(r.bytes_from_ring, 4);
        assert!(r.underrun);
        // First 4 are real, last 4 are silence (not the 0xAA they held).
        assert_eq!(scratch, [9, 9, 9, 9, 0, 0, 0, 0]);
    }

    #[test]
    fn empty_ring_is_all_silence() {
        let (_p, mut c) = RingBuffer::<u8>::new(16);
        let mut scratch = [0x55u8; 8];
        let r = pull_into(&mut c, &mut scratch);
        assert_eq!(r.bytes_from_ring, 0);
        assert!(r.underrun);
        assert_eq!(scratch, [0u8; 8]);
    }

    #[test]
    fn pull_across_ring_wrap_is_contiguous() {
        // Capacity 8; write 6, read 6, write 6 more so the data wraps the buffer.
        let (mut p, mut c) = RingBuffer::<u8>::new(8);
        p.push_entire_slice(&[1, 2, 3, 4, 5, 6]).unwrap();
        let mut throwaway = [0u8; 6];
        let _ = c.pop_partial_slice(&mut throwaway);
        p.push_entire_slice(&[10, 11, 12, 13, 14, 15]).unwrap();
        let mut scratch = [0u8; 6];
        let r = pull_into(&mut c, &mut scratch);
        assert_eq!(r.bytes_from_ring, 6);
        assert!(!r.underrun);
        assert_eq!(scratch, [10, 11, 12, 13, 14, 15]);
    }

    #[test]
    fn service_flush_no_request_is_noop() {
        let (mut p, mut c) = RingBuffer::<u8>::new(16);
        p.push_entire_slice(&[1, 2, 3, 4]).unwrap();
        let req = AtomicU64::new(0);
        let ack = AtomicU64::new(0);
        let mut scratch = [0u8; 8];
        let serviced = service_flush(&mut c, &mut scratch, &req, &ack);
        assert!(!serviced, "no pending request -> no-op");
        assert_eq!(ack.load(Ordering::Acquire), 0);
        // The bytes are untouched: a normal pull still sees them.
        assert_eq!(c.slots(), 4);
    }

    #[test]
    fn service_flush_drains_ring_and_acks() {
        // Ring holds more than one scratch-full so the drain loop must iterate.
        let (mut p, mut c) = RingBuffer::<u8>::new(64);
        let data: Vec<u8> = (0..50u8).collect();
        p.push_entire_slice(&data).unwrap();
        let req = AtomicU64::new(1); // owner requested generation 1
        let ack = AtomicU64::new(0);
        let mut scratch = [0u8; 8]; // smaller than the ring contents
        let serviced = service_flush(&mut c, &mut scratch, &req, &ack);
        assert!(serviced, "pending request -> serviced, caller suppresses underrun");
        assert_eq!(c.slots(), 0, "ring fully drained");
        assert_eq!(ack.load(Ordering::Acquire), 1, "ack bumped to requested gen");
        // A second call with no further request is a no-op.
        assert!(!service_flush(&mut c, &mut scratch, &req, &ack));
    }

    #[test]
    fn service_flush_acks_only_observed_generation() {
        let (_p, mut c) = RingBuffer::<u8>::new(16);
        let req = AtomicU64::new(3);
        let ack = AtomicU64::new(1);
        let mut scratch = [0u8; 8];
        assert!(service_flush(&mut c, &mut scratch, &req, &ack));
        assert_eq!(ack.load(Ordering::Acquire), 3, "collapses skipped generations");
    }

    // --- FlushGrace: post-flush reprime accounting (ADR-013 extension) -------

    /// Device buffer used by the grace tests: 8 bytes. `low_water` is one whole
    /// device buffer, matching the backend's `buffer_frames * bytes_per_frame`.
    const DEV_BUF: usize = 8;

    #[test]
    fn grace_suppresses_post_flush_reprime_gap() {
        // After a flush the ring stays empty for several feeder iterations while a
        // slow producer reprimes. Underruns must stay 0 throughout, and grace must
        // exit (resume counting) once the ring refills past the low-water mark.
        let (mut p, mut c) = RingBuffer::<u8>::new(64);
        let req = AtomicU64::new(0);
        let ack = AtomicU64::new(0);
        let mut scratch = [0u8; DEV_BUF];
        // cap huge so this test exercises the low-water exit, not the safety cap.
        let mut grace = FlushGrace::new(DEV_BUF, 1000);
        let mut underruns = 0u64;

        // Steady playback had queued a buffer; the owner now requests a seek flush.
        p.push_entire_slice(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        req.store(1, Ordering::Release);

        // Flush iteration: service_flush drains the ring, the pull renders silence.
        let flushed = service_flush(&mut c, &mut scratch, &req, &ack);
        assert!(flushed);
        let ring_level = c.slots();
        let pull = pull_into(&mut c, &mut scratch);
        assert!(pull.underrun, "ring drained by the flush -> silence");
        if grace.count_underrun(flushed, ring_level, pull.underrun) {
            underruns += 1;
        }
        assert_eq!(underruns, 0, "the flush iteration is a deliberate discard");

        // Reprime in flight: producer still decoding, ring stays empty.
        for _ in 0..5 {
            let flushed = service_flush(&mut c, &mut scratch, &req, &ack);
            assert!(!flushed, "no new flush pending during the reprime");
            let ring_level = c.slots();
            let pull = pull_into(&mut c, &mut scratch);
            assert!(pull.underrun);
            if grace.count_underrun(flushed, ring_level, pull.underrun) {
                underruns += 1;
            }
        }
        assert_eq!(underruns, 0, "the reprime gap is suppressed while in grace");
        assert!(grace.in_grace(), "still waiting for the reprime");

        // Reprime lands: the producer refills past the low-water mark.
        p.push_entire_slice(&[9, 9, 9, 9, 9, 9, 9, 9]).unwrap();
        let flushed = service_flush(&mut c, &mut scratch, &req, &ack);
        let ring_level = c.slots();
        assert!(ring_level >= DEV_BUF, "a full device buffer is queued again");
        let pull = pull_into(&mut c, &mut scratch);
        assert!(!pull.underrun, "the pull now fills completely");
        if grace.count_underrun(flushed, ring_level, pull.underrun) {
            underruns += 1;
        }
        assert_eq!(underruns, 0);
        assert!(!grace.in_grace(), "reprime landed -> grace exits, counting resumes");
    }

    #[test]
    fn grace_does_not_suppress_mid_playback_underrun() {
        // No flush ever happens. An empty ring during normal playback must count an
        // underrun every iteration: grace must not over-suppress the perf path.
        let (_p, mut c) = RingBuffer::<u8>::new(64);
        let mut scratch = [0u8; DEV_BUF];
        let mut grace = FlushGrace::new(DEV_BUF, 1000);
        let mut underruns = 0u64;

        for _ in 0..4 {
            let ring_level = c.slots();
            let pull = pull_into(&mut c, &mut scratch);
            assert!(pull.underrun);
            // flushed = false: steady-state starvation, never a deliberate gap.
            if grace.count_underrun(false, ring_level, pull.underrun) {
                underruns += 1;
            }
        }
        assert_eq!(underruns, 4, "steady-state starvation is never masked");
        assert!(!grace.in_grace());
    }

    #[test]
    fn grace_safety_cap_surfaces_a_stalled_producer() {
        // A flush arms grace, but the producer never reprimes. After the bounded cap
        // of grace iterations, underruns surface again (grace is not unbounded).
        let (_p, mut c) = RingBuffer::<u8>::new(64);
        let req = AtomicU64::new(1);
        let ack = AtomicU64::new(0);
        let mut scratch = [0u8; DEV_BUF];
        let cap = 3u32;
        let mut grace = FlushGrace::new(DEV_BUF, cap);
        let mut underruns = 0u64;

        // Flush iteration arms grace.
        let flushed = service_flush(&mut c, &mut scratch, &req, &ack);
        assert!(flushed);
        let ring_level = c.slots();
        let pull = pull_into(&mut c, &mut scratch);
        if grace.count_underrun(flushed, ring_level, pull.underrun) {
            underruns += 1;
        }

        // Producer never reprimes: the ring stays empty forever.
        for _ in 0..(cap as usize + 2) {
            let ring_level = c.slots();
            assert_eq!(ring_level, 0, "producer stalled: nothing reprimed");
            let pull = pull_into(&mut c, &mut scratch);
            if grace.count_underrun(false, ring_level, pull.underrun) {
                underruns += 1;
            }
        }
        // `cap` iterations are suppressed; the remaining 2 surface as underruns.
        assert_eq!(underruns, 2, "a producer that never reprimes still surfaces underruns");
        assert!(!grace.in_grace(), "grace lapsed once the safety cap elapsed");
    }

    #[test]
    fn silence_zeroes_buffer() {
        let mut scratch = [0xFFu8; 5];
        silence(&mut scratch);
        assert_eq!(scratch, [0u8; 5]);
    }

    #[test]
    fn dop_idle_alternates_markers_per_frame() {
        // Stereo, two frames (2 ch * 4 bytes * 2 frames = 16 bytes).
        let mut scratch = [0xFFu8; 16];
        let next = dop_idle(&mut scratch, 2, true);
        // Frame 0: marker 0x05 on both channels' top byte.
        assert_eq!(scratch[0..4], [0x00, 0x69, 0x69, 0x05]);
        assert_eq!(scratch[4..8], [0x00, 0x69, 0x69, 0x05]);
        // Frame 1: marker flips to 0xFA.
        assert_eq!(scratch[8..12], [0x00, 0x69, 0x69, 0xFA]);
        assert_eq!(scratch[12..16], [0x00, 0x69, 0x69, 0xFA]);
        // Next call should resume at 0x05 (two frames = even flips).
        assert!(next);
    }

    #[test]
    fn dop_idle_marker_state_continues_across_buffers() {
        // One stereo frame per buffer; the marker state must carry over.
        let mut buf = [0u8; 8];
        let s1 = dop_idle(&mut buf, 2, true);
        assert_eq!(buf[3], 0x05);
        assert!(!s1); // after one frame, next starts at 0xFA
        let s2 = dop_idle(&mut buf, 2, s1);
        assert_eq!(buf[3], 0xFA);
        assert!(s2);
    }
}
