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

use rtrb::Consumer;

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
/// (DoP needs a non-zero idle pattern instead; that lands with the DoP packer in
/// P5 and will override this for DSD streams.)
pub fn silence(scratch: &mut [u8]) {
    for b in scratch.iter_mut() {
        *b = 0;
    }
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
    fn silence_zeroes_buffer() {
        let mut scratch = [0xFFu8; 5];
        silence(&mut scratch);
        assert_eq!(scratch, [0u8; 5]);
    }
}
