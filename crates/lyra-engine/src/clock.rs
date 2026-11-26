//! The position clock: device frames consumed → source frames.
//!
//! Playback position is reported from the frames the **device has actually
//! consumed** (`OutputStats::frames_played`), not from frames decoded. For a plain
//! PCM path the device rate equals the source rate, so the mapping is one-to-one.
//! For DSD → PCM the device plays decimated PCM while the source is counted in DSD
//! samples per channel, so the clock scales by `src_hz / dev_hz` (e.g. DSD64 at
//! 2 822 400 Hz over a 176 400 Hz carrier → ×16).
//!
//! The clock is a pure value type (no threads, no I/O) so the mapping — including
//! the rebase after a seek or a track change — is unit-tested directly.

/// Maps device frames-played to source frames for the current playing segment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PositionClock {
    /// Source frames per second (PCM sample rate, or DSD samples/channel per sec).
    src_hz: u64,
    /// Device frames per second (the negotiated PCM carrier rate).
    dev_hz: u64,
    /// Source frame at the start of the current segment (0, or a seek target).
    base_src: u64,
    /// Device frames-played snapshot captured when the segment began.
    base_dev: u64,
}

impl PositionClock {
    /// A clock for a segment that plays `src_hz` source frames per second through a
    /// device consuming `dev_hz` frames per second. Both must be non-zero.
    pub(crate) fn new(src_hz: u64, dev_hz: u64) -> Self {
        Self { src_hz: src_hz.max(1), dev_hz: dev_hz.max(1), base_src: 0, base_dev: 0 }
    }

    /// Re-anchor the clock: from now on `device_frames_played == base_dev`
    /// corresponds to source frame `base_src`. Called when a track starts (0) and
    /// after a seek (the landed frame).
    pub(crate) fn rebase(&mut self, base_src: u64, base_dev: u64) {
        self.base_src = base_src;
        self.base_dev = base_dev;
    }

    /// The source frame currently at the device, given its frames-played counter.
    pub(crate) fn source_frame(&self, device_frames_played: u64) -> u64 {
        let delta_dev = device_frames_played.saturating_sub(self.base_dev);
        let scaled = (u128::from(delta_dev) * u128::from(self.src_hz)) / u128::from(self.dev_hz);
        self.base_src.saturating_add(scaled as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_is_one_to_one() {
        let clock = PositionClock::new(44_100, 44_100);
        assert_eq!(clock.source_frame(0), 0);
        assert_eq!(clock.source_frame(44_100), 44_100);
        assert_eq!(clock.source_frame(1_234), 1_234);
    }

    #[test]
    fn dsd_to_pcm_scales_by_ratio() {
        // DSD64 (2_822_400) over a 176_400 Hz carrier → ×16.
        let clock = PositionClock::new(2_822_400, 176_400);
        assert_eq!(clock.source_frame(0), 0);
        assert_eq!(clock.source_frame(1), 16);
        assert_eq!(clock.source_frame(176_400), 2_822_400);
    }

    #[test]
    fn rebase_after_seek_offsets_from_landed_frame() {
        let mut clock = PositionClock::new(48_000, 48_000);
        // Device has already consumed 100_000 frames of earlier audio.
        clock.rebase(500_000, 100_000);
        // Right at the seek the position is the landed frame.
        assert_eq!(clock.source_frame(100_000), 500_000);
        // 48_000 device frames later the position has advanced one second.
        assert_eq!(clock.source_frame(148_000), 548_000);
    }

    #[test]
    fn rebase_for_new_track_counts_from_running_device_counter() {
        // frames_played never resets while the device stays open across a gapless
        // join, so a new track rebases to the current counter.
        let mut clock = PositionClock::new(96_000, 96_000);
        clock.rebase(0, 2_000_000);
        assert_eq!(clock.source_frame(2_000_000), 0);
        assert_eq!(clock.source_frame(2_096_000), 96_000);
    }
}
