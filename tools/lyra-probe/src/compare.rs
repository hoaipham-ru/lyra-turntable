//! The core QA primitive: compare two valid-bit-justified PCM sample streams.
//!
//! Everything here is pure and buffer-based so it can be unit-tested without any
//! audio files (see the `tests` module). Byte-level unpacking and the ADR-004
//! padding rule live in [`crate::pcm`]; by the time samples reach this module
//! both streams are already `i32` at the same `bits` scale.

use serde::Serialize;

/// Full-scale sample magnitude for a given valid bit depth: `2^(bits-1)`.
#[must_use]
pub fn full_scale(bits: u32) -> f64 {
    2f64.powi(i32::try_from(bits).unwrap_or(0) - 1)
}

/// Outcome of comparing two PCM streams, serialisable to the report JSON.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CompareResult {
    /// Both streams are the same length and every sample is identical.
    pub bit_exact: bool,
    /// Same length and every sample within `tolerance` LSBs (implies the files
    /// pass a tolerance-based check; `bit_exact` is the `tolerance == 0` case).
    pub within_tolerance: bool,
    /// The two streams have different sample counts.
    pub length_mismatch: bool,
    /// Frame count of stream A (`samples / channels`).
    pub frames_a: usize,
    /// Frame count of stream B.
    pub frames_b: usize,
    /// Index of the first frame containing a sample that differs by more than
    /// `tolerance` LSBs, or `None` if none within the compared span.
    pub first_diff_frame: Option<usize>,
    /// Index of the first differing sample (interleaved), or `None`.
    pub first_diff_sample: Option<usize>,
    /// Largest absolute sample difference over the compared span, in LSBs.
    pub max_abs_diff: i64,
    /// RMS of the difference signal expressed in dBFS (the "null depth").
    /// `None` encodes a perfect null (−inf dB) so it round-trips through JSON.
    pub null_depth_dbfs: Option<f64>,
    /// Valid bit depth both streams were compared at.
    pub bits: u32,
    /// Number of interleaved channels.
    pub channels: usize,
    /// Tolerance (LSBs) used for the pass/fail decision.
    pub tolerance: i64,
}

/// Compare two valid-bit-justified PCM streams.
///
/// `a` and `b` are interleaved `i32` samples already normalised to the same
/// `bits` scale (see [`crate::pcm::unpack`]). The comparison is made over the
/// overlapping prefix `min(a.len(), b.len())`; a length difference sets
/// `length_mismatch`.
///
/// `tolerance` is the maximum absolute per-sample difference (in LSBs) still
/// treated as a match for `within_tolerance` and for locating the first
/// differing frame. `bit_exact` ignores tolerance.
///
/// # Panics
/// Panics if `channels == 0`.
#[must_use]
pub fn compare_pcm(a: &[i32], b: &[i32], channels: usize, bits: u32, tolerance: i64) -> CompareResult {
    assert!(channels > 0, "channels must be non-zero");
    let n = a.len().min(b.len());
    let length_mismatch = a.len() != b.len();

    let mut max_abs_diff: i64 = 0;
    let mut sum_sq: f64 = 0.0;
    let mut first_diff_sample: Option<usize> = None;

    for i in 0..n {
        let d = i64::from(a[i]) - i64::from(b[i]);
        let ad = d.abs();
        if ad > max_abs_diff {
            max_abs_diff = ad;
        }
        let df = d as f64;
        sum_sq += df * df;
        if ad > tolerance && first_diff_sample.is_none() {
            first_diff_sample = Some(i);
        }
    }

    // A length mismatch is itself a first difference at the end of the prefix,
    // provided nothing earlier already differed.
    if length_mismatch && first_diff_sample.is_none() {
        first_diff_sample = Some(n);
    }

    let null_depth_dbfs = if sum_sq == 0.0 || n == 0 {
        None // perfect null over the compared span
    } else {
        let rms = (sum_sq / n as f64).sqrt();
        Some(20.0 * (rms / full_scale(bits)).log10())
    };

    let bit_exact = !length_mismatch && max_abs_diff == 0;
    let within_tolerance = !length_mismatch && max_abs_diff <= tolerance;

    CompareResult {
        bit_exact,
        within_tolerance,
        length_mismatch,
        frames_a: a.len() / channels,
        frames_b: b.len() / channels,
        first_diff_frame: first_diff_sample.map(|s| s / channels),
        first_diff_sample,
        max_abs_diff,
        null_depth_dbfs,
        bits,
        channels,
        tolerance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_exact_match() {
        let a = vec![0, 100, -100, 32767, -32768, 5, 6, 7];
        let b = a.clone();
        let r = compare_pcm(&a, &b, 2, 16, 0);
        assert!(r.bit_exact);
        assert!(r.within_tolerance);
        assert!(!r.length_mismatch);
        assert_eq!(r.max_abs_diff, 0);
        assert_eq!(r.first_diff_frame, None);
        assert_eq!(r.first_diff_sample, None);
        assert_eq!(r.null_depth_dbfs, None); // perfect null
        assert_eq!(r.frames_a, 4);
    }

    #[test]
    fn off_by_one_lsb_within_tolerance() {
        let a = vec![0, 100, 200, 300];
        let b = vec![1, 99, 201, 299]; // every sample off by exactly 1 LSB
        let within = compare_pcm(&a, &b, 2, 16, 1);
        assert!(!within.bit_exact);
        assert!(within.within_tolerance);
        assert_eq!(within.max_abs_diff, 1);

        // With zero tolerance the same data fails and reports the first diff.
        let strict = compare_pcm(&a, &b, 2, 16, 0);
        assert!(!strict.bit_exact);
        assert!(!strict.within_tolerance);
        assert_eq!(strict.first_diff_sample, Some(0));
        assert_eq!(strict.first_diff_frame, Some(0));
    }

    #[test]
    fn first_diff_index_located() {
        // Stereo: samples 0..=5 equal, sample 6 (frame 3, left) differs by 4.
        let a = vec![10, 11, 12, 13, 14, 15, 16, 17];
        let mut b = a.clone();
        b[6] = 20;
        let r = compare_pcm(&a, &b, 2, 16, 1);
        assert_eq!(r.first_diff_sample, Some(6));
        assert_eq!(r.first_diff_frame, Some(3));
        assert_eq!(r.max_abs_diff, 4);
        assert!(!r.within_tolerance);
    }

    #[test]
    fn null_depth_of_known_constant_diff() {
        // A constant 1-LSB difference on a 16-bit scale: RMS = 1,
        // full scale = 2^15 = 32768, so 20*log10(1/32768) ≈ -90.31 dBFS.
        let a = vec![0i32; 1000];
        let b = vec![1i32; 1000];
        let r = compare_pcm(&a, &b, 2, 16, 0);
        let db = r.null_depth_dbfs.expect("non-perfect null");
        assert!((db - (-90.3089)).abs() < 1e-3, "got {db}");
        assert_eq!(r.max_abs_diff, 1);
    }

    #[test]
    fn length_mismatch_flagged() {
        let a = vec![1, 2, 3, 4, 5, 6];
        let b = vec![1, 2, 3, 4]; // B is shorter
        let r = compare_pcm(&a, &b, 2, 16, 0);
        assert!(r.length_mismatch);
        assert!(!r.bit_exact);
        // Prefix matches, so the first difference is at the end of the overlap.
        assert_eq!(r.first_diff_sample, Some(4));
        assert_eq!(r.frames_a, 3);
        assert_eq!(r.frames_b, 2);
    }

    #[test]
    fn full_scale_values() {
        assert_eq!(full_scale(16), 32768.0);
        assert_eq!(full_scale(24), 8_388_608.0);
    }
}
