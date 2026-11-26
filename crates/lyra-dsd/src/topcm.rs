//! DSD → PCM conversion (S03, ADR-009) — the path for a DAC without DSD/DoP.
//!
//! This is a **"processed"** path (ADR-004): the 1-bit stream is low-pass
//! filtered and decimated to 24-bit PCM, so the output is measured (THD+N,
//! passband, noise floor), never bit-compared.
//!
//! Pipeline, per channel:
//! 1. Map each DSD bit to ±1.0 (`f64`).
//! 2. Linear-phase FIR low-pass (Kaiser-windowed sinc) run at the DSD rate.
//! 3. Decimate to the target PCM rate (88.2 or 176.4 kHz) by taking one filtered
//!    output every `decim` DSD samples.
//! 4. Quantise to signed 24-bit with TPDF dither (word-length reduction only).
//!
//! The FIR is designed with the audio band (to 20 kHz) as the pass band and the
//! target Nyquist as the stop band, so the enormous out-of-band DSD shaped noise
//! is suppressed before it can alias into the audio band. All filter state is
//! held across [`DsdToPcm::process`] calls, so there is no discontinuity at
//! buffer boundaries; [`DsdToPcm::reset`] clears it at a track join.

use std::f64::consts::PI;

use lyra_core::{DsdSpec, LyraError, PcmSpec, Result, SampleFormat};

/// The PCM sample rate a DSD stream is decimated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRate {
    /// 88.2 kHz (DSD64 ÷ 32).
    Pcm88_2,
    /// 176.4 kHz (DSD64 ÷ 16).
    Pcm176_4,
}

impl TargetRate {
    /// The target rate in Hz.
    #[must_use]
    pub const fn hz(self) -> u32 {
        match self {
            TargetRate::Pcm88_2 => 88_200,
            TargetRate::Pcm176_4 => 176_400,
        }
    }
}

/// Pass-band edge in Hz: the response is flat to here (audio band).
const PASSBAND_HZ: f64 = 20_000.0;
/// Target FIR stop-band attenuation in dB (≥ 10 dB below the −110 dBFS floor
/// we must stay under, so folded noise cannot lift the audio-band floor).
const STOPBAND_DB: f64 = 120.0;

/// A streaming DSD → PCM converter.
pub struct DsdToPcm {
    channels: usize,
    msb_first: bool,
    decim: usize,
    target_rate: u32,
    /// Symmetric FIR taps (DC gain normalised to 1).
    coeffs: Vec<f64>,
    taps: usize,
    /// Per-channel ring buffer of the last `taps` input samples
    /// (`channels * taps`, channel `ch` occupies `[ch*taps, ch*taps+taps)`).
    hist: Vec<f64>,
    /// Shared write index into each channel's ring (all channels advance together).
    widx: usize,
    /// Decimation phase: an output is produced when this reaches `decim`.
    phase: usize,
    /// Leftover interleaved input bytes (`< channels`) awaiting a full byte-group.
    pending: Vec<u8>,
    /// Deterministic PRNG state for the TPDF dither.
    rng: u64,
}

impl DsdToPcm {
    /// Create a converter for `spec` targeting `target`.
    ///
    /// # Errors
    /// Returns [`LyraError::UnsupportedFormat`] if the DSD rate is not an integer
    /// multiple of the target rate (so decimation would not be exact).
    pub fn new(spec: DsdSpec, target: TargetRate) -> Result<Self> {
        let dsd_hz = spec.rate.hz();
        let target_hz = target.hz();
        if !dsd_hz.is_multiple_of(target_hz) {
            return Err(LyraError::UnsupportedFormat(format!(
                "DSD {dsd_hz} Hz is not an integer multiple of target {target_hz} Hz"
            )));
        }
        let decim = (dsd_hz / target_hz) as usize;
        if decim < 2 {
            return Err(LyraError::UnsupportedFormat(
                "decimation factor must be at least 2".to_string(),
            ));
        }

        let fs = f64::from(dsd_hz);
        let fstop = f64::from(target_hz) / 2.0; // target Nyquist: nothing above may pass
        let coeffs = design_lowpass(fs, PASSBAND_HZ, fstop, STOPBAND_DB);
        let taps = coeffs.len();
        let channels = spec.channels as usize;

        Ok(DsdToPcm {
            channels,
            msb_first: spec.msb_first,
            decim,
            target_rate: target_hz,
            coeffs,
            taps,
            hist: vec![0.0; channels * taps],
            widx: 0,
            phase: 0,
            pending: Vec::new(),
            rng: 0x9E37_79B9_7F4A_7C15,
        })
    }

    /// The PCM format this converter produces (24-bit, `target` Hz).
    #[must_use]
    pub fn out_spec(&self) -> PcmSpec {
        PcmSpec {
            rate: self.target_rate,
            channels: self.channels as u16,
            bits_valid: 24,
            format: SampleFormat::S24,
        }
    }

    /// The FIR taps, exposed for QA/analysis (e.g. passband-ripple checks).
    #[must_use]
    pub fn filter_taps(&self) -> &[f64] {
        &self.coeffs
    }

    /// Clear all filter state and buffered input (use at a track join).
    pub fn reset(&mut self) {
        self.hist.iter_mut().for_each(|s| *s = 0.0);
        self.widx = 0;
        self.phase = 0;
        self.pending.clear();
        self.rng = 0x9E37_79B9_7F4A_7C15;
    }

    /// Convert `dsd` (interleaved native-bit-order DSD bytes) to 24-bit PCM,
    /// appending little-endian `S24` frames to `out`.
    ///
    /// Input that does not complete a byte-group (`channels` bytes) is buffered
    /// for the next call, so output is identical however the input is chunked.
    pub fn process(&mut self, dsd: &[u8], out: &mut Vec<u8>) {
        let group = self.channels;
        if group == 0 {
            return;
        }

        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(dsd);
        let full_groups = buf.len() / group;

        for g in 0..full_groups {
            let base = g * group;
            for bit in 0..8 {
                for ch in 0..self.channels {
                    let byte = buf[base + ch];
                    let b = if self.msb_first {
                        (byte >> (7 - bit)) & 1
                    } else {
                        (byte >> bit) & 1
                    };
                    let s = if b == 1 { 1.0 } else { -1.0 };
                    self.hist[ch * self.taps + self.widx] = s;
                }
                self.widx = if self.widx + 1 == self.taps { 0 } else { self.widx + 1 };
                self.phase += 1;
                if self.phase == self.decim {
                    self.phase = 0;
                    for ch in 0..self.channels {
                        let y = self.output_sample(ch);
                        let q = self.quantize(y);
                        out.push((q & 0xFF) as u8);
                        out.push(((q >> 8) & 0xFF) as u8);
                        out.push(((q >> 16) & 0xFF) as u8);
                    }
                }
            }
        }

        let consumed = full_groups * group;
        buf.drain(0..consumed);
        self.pending = buf;
    }

    /// FIR dot product for channel `ch` at the current history position.
    fn output_sample(&self, ch: usize) -> f64 {
        let base = ch * self.taps;
        // Newest sample sits just before `widx`; walk backwards through the ring.
        let mut idx = if self.widx == 0 { self.taps - 1 } else { self.widx - 1 };
        let mut acc = 0.0;
        for &c in &self.coeffs {
            acc += c * self.hist[base + idx];
            idx = if idx == 0 { self.taps - 1 } else { idx - 1 };
        }
        acc
    }

    /// Quantise a value in roughly [-1, 1] to signed 24-bit with TPDF dither.
    fn quantize(&mut self, v: f64) -> i32 {
        const FULL: f64 = 8_388_607.0; // 2^23 - 1
        let dither = self.tpdf(); // triangular, ±1 LSB peak
        let q = (v * FULL + dither).round();
        q.clamp(-8_388_608.0, 8_388_607.0) as i32
    }

    /// One TPDF (triangular) dither sample, peak ±1 LSB.
    fn tpdf(&mut self) -> f64 {
        self.next_uniform() - self.next_uniform()
    }

    /// Next uniform in [0, 1) from a SplitMix64 step (deterministic).
    fn next_uniform(&mut self) -> f64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // top 53 bits -> [0,1)
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Design a linear-phase (Type I) Kaiser-windowed-sinc low-pass filter.
///
/// `fp`/`fstop` are the pass/stop band edges in Hz, `fs` the sample rate, and
/// `atten_db` the target stop-band attenuation. The returned taps are symmetric
/// with DC gain normalised to 1.
fn design_lowpass(fs: f64, fp: f64, fstop: f64, atten_db: f64) -> Vec<f64> {
    let fc = 0.5 * (fp + fstop); // -6 dB cutoff, midway in the transition
    let trans = 2.0 * PI * (fstop - fp) / fs; // transition width (rad/sample)
    let mut n = (((atten_db - 8.0) / (2.285 * trans)).ceil() as usize).max(1);
    if n.is_multiple_of(2) {
        n += 1; // odd length -> exact linear phase, integer group delay
    }
    let beta = if atten_db > 50.0 {
        0.1102 * (atten_db - 8.7)
    } else if atten_db >= 21.0 {
        0.5842 * (atten_db - 21.0).powf(0.4) + 0.078_86 * (atten_db - 21.0)
    } else {
        0.0
    };

    let m = (n - 1) as f64;
    let wc = 2.0 * PI * fc / fs;
    let i0_beta = bessel_i0(beta);
    let mut h = vec![0.0f64; n];
    for (k, hk) in h.iter_mut().enumerate() {
        let x = k as f64 - m / 2.0;
        let sinc = if x.abs() < 1e-9 { wc / PI } else { (wc * x).sin() / (PI * x) };
        let r = 2.0 * k as f64 / m - 1.0; // -1..=1
        let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;
        *hk = sinc * w;
    }
    let sum: f64 = h.iter().sum();
    if sum.abs() > f64::EPSILON {
        for v in &mut h {
            *v /= sum;
        }
    }
    h
}

/// Modified Bessel function of the first kind, order 0.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let mut k = 1.0;
    loop {
        let t = x / (2.0 * k);
        term *= t * t;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
        k += 1.0;
        if k > 1000.0 {
            break;
        }
    }
    sum
}
