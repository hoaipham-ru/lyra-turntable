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
