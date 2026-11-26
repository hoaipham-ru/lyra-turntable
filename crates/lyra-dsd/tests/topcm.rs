//! DSD -> PCM conversion measurements (S03, P5.6): noise floor, passband,
//! chunk-boundary continuity.

use std::f64::consts::PI;

use lyra_core::{DsdRate, DsdSpec};
use lyra_dsd::{DsdToPcm, TargetRate};

// --- synthetic DSD source -----------------------------------------------------

/// A 2nd-order 1-bit sigma-delta modulator (unconditionally stable for |x| well
/// below 1). Emits one bit per call; feed a band-limited input and the in-band
/// quantisation noise is shaped up out of the audio band.
struct Modulator {
    e1: f64,
    e2: f64,
    q: f64,
}

impl Modulator {
    fn new() -> Self {
        Modulator { e1: 0.0, e2: 0.0, q: 1.0 }
    }
    fn step(&mut self, x: f64) -> u8 {
        self.e1 += x - self.q;
        self.e2 += self.e1 - self.q;
        self.q = if self.e2 >= 0.0 { 1.0 } else { -1.0 };
        u8::from(self.q > 0.0)
    }
}

/// Generate `n_samples` mono DSD bytes of a `freq`-Hz sine at `amp` (relative to
/// full scale), packed MSB-first (DFF-style). `n_samples` must be a multiple of 8.
fn make_tone(freq: f64, amp: f64, n_samples: usize, dsd_hz: f64) -> Vec<u8> {
    assert_eq!(n_samples % 8, 0);
    let mut m = Modulator::new();
    let mut bytes = Vec::with_capacity(n_samples / 8);
    let mut byte = 0u8;
    for i in 0..n_samples {
        let t = i as f64 / dsd_hz;
        let x = amp * (2.0 * PI * freq * t).sin();
        let bit = m.step(x);
        byte = (byte << 1) | bit; // MSB-first: first sample ends up in bit 7
        if i % 8 == 7 {
            bytes.push(byte);
            byte = 0;
        }
    }
    bytes
}

// --- minimal FFT --------------------------------------------------------------

/// In-place iterative radix-2 Cooley-Tukey FFT. `re.len()` must be a power of 2.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    assert!(n.is_power_of_two());
    // bit-reversal permutation
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        let half = len / 2;
        let mut i = 0;
        while i < n {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..half {
                let a = i + k;
                let b = i + k + half;
                let tr = cr * re[b] - ci * im[b];
                let ti = cr * im[b] + ci * re[b];
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
            i += len;
        }
        len <<= 1;
    }
}

/// Unpack little-endian packed S24 bytes to f64 in [-1, 1).
fn unpack_s24(bytes: &[u8]) -> Vec<f64> {
    bytes
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| {
            let raw = (i32::from(c[0])) | (i32::from(c[1]) << 8) | (i32::from(c[2]) << 16);
            let signed = (raw << 8) >> 8; // sign-extend 24 -> 32
            f64::from(signed) / 8_388_608.0
        })
        .collect()
}

// --- tests --------------------------------------------------------------------

#[test]
fn dsd_to_pcm_noise_floor_and_passband_88k() {
    run_tone_measurement(TargetRate::Pcm88_2, 88_200.0);
}

#[test]
fn dsd_to_pcm_noise_floor_and_passband_176k() {
    run_tone_measurement(TargetRate::Pcm176_4, 176_400.0);
}

fn run_tone_measurement(target: TargetRate, out_hz: f64) {
    let dsd_hz = DsdRate::Dsd64.hz() as f64;
    let decim = (dsd_hz as u32 / target.hz()) as usize;

    // Scale the FFT length with the output rate so both rates are measured at the
    // same bin width (~2.7 Hz); the per-bin noise floor in dBFS depends on bin
    // width, so holding it constant makes the two rates directly comparable.
    let ref_bin_hz = 88_200.0 / 32_768.0;
    let n_fft = ((out_hz / ref_bin_hz).round() as usize).next_power_of_two();
    let warmup_out = 4_000usize; // discard filter/modulator transient
    let n_out = n_fft + warmup_out;
    let mut n_dsd = n_out * decim;
    n_dsd += (8 - n_dsd % 8) % 8; // round up to a whole byte

    // Put the tone (~1 kHz) exactly on an FFT bin to avoid leakage.
    let bin = (1_000.0 * n_fft as f64 / out_hz).round() as usize;
    let tone_hz = bin as f64 * out_hz / n_fft as f64;
    let amp = 0.5; // -6 dBFS

    let dsd = make_tone(tone_hz, amp, n_dsd, dsd_hz);
    let spec = DsdSpec { rate: DsdRate::Dsd64, channels: 1, msb_first: true };
    let mut conv = DsdToPcm::new(spec, target).unwrap();
    let mut pcm = Vec::new();
    conv.process(&dsd, &mut pcm);

    let samples = unpack_s24(&pcm);
    assert!(samples.len() >= n_out, "produced {} samples", samples.len());

    // Window a block past the warmup with a Hann window.
    let start = warmup_out;
    let mut re = vec![0.0f64; n_fft];
    let mut im = vec![0.0f64; n_fft];
    let mut winsum = 0.0f64;
    for i in 0..n_fft {
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / n_fft as f64).cos();
        winsum += w;
        re[i] = samples[start + i] * w;
    }
    fft(&mut re, &mut im);

    // Normalise so a full-scale on-bin sine reads 0 dBFS.
    // For a Hann window, a sine of amplitude A reads |X| = A * winsum / 2 at its bin.
    let ref_mag = winsum / 2.0;
    let mag_db = |k: usize| {
        let p = (re[k] * re[k] + im[k] * im[k]).sqrt() / ref_mag;
        20.0 * p.max(1e-30).log10()
    };

    let tone_db = mag_db(bin);

    // Noise floor: average power over 20 Hz..20 kHz, excluding the tone bins.
    let lo = (20.0 * n_fft as f64 / out_hz).ceil() as usize;
    let hi = (20_000.0 * n_fft as f64 / out_hz).floor() as usize;
    let guard = 3usize;
    let mut sum_pow = 0.0f64;
    let mut count = 0usize;
    let mut max_db = f64::NEG_INFINITY;
    for k in lo..=hi {
        if k.abs_diff(bin) <= guard {
            continue;
        }
        let d = mag_db(k);
        sum_pow += 10f64.powf(d / 10.0);
        count += 1;
        if d > max_db {
            max_db = d;
        }
    }
    let floor_db = 10.0 * (sum_pow / count as f64).log10();

    eprintln!(
        "target={} Hz tone={:.1} Hz tone_db={:.2} floor_db={:.2} max_bin_db={:.2}",
        target.hz(),
        tone_hz,
        tone_db,
        floor_db,
        max_db
    );

    // Tone recovered near -6 dBFS.
    assert!((-7.0..=-5.0).contains(&tone_db), "tone level {tone_db:.2} dBFS");
    // Average noise floor below -110 dBFS in the audio band.
    assert!(floor_db < -110.0, "noise floor {floor_db:.2} dBFS >= -110");
}

#[test]
fn passband_ripple_small_to_20k() {
    let spec = DsdSpec { rate: DsdRate::Dsd64, channels: 1, msb_first: true };
    let conv = DsdToPcm::new(spec, TargetRate::Pcm88_2).unwrap();
    let taps = conv.filter_taps();
    let fs = DsdRate::Dsd64.hz() as f64;

    // FIR magnitude response at DC and across the audio band.
    let resp_db = |f: f64| {
        let mut re = 0.0f64;
        let mut im = 0.0f64;
        for (n, &c) in taps.iter().enumerate() {
            let ph = -2.0 * PI * f * n as f64 / fs;
            re += c * ph.cos();
            im += c * ph.sin();
        }
        20.0 * (re * re + im * im).sqrt().max(1e-30).log10()
    };

    let dc = resp_db(0.0);
    assert!(dc.abs() < 1e-6, "DC gain must be unity, got {dc} dB");
    for f in [1_000.0, 5_000.0, 10_000.0, 15_000.0, 20_000.0] {
        let r = resp_db(f);
        assert!(r.abs() <= 0.1, "ripple at {f} Hz is {r:.4} dB (> 0.1)");
    }
    // Strong attenuation at the target Nyquist (44.1 kHz).
    assert!(resp_db(44_100.0) < -100.0, "weak stopband at Nyquist");
}

#[test]
fn chunk_boundary_continuity() {
    // One-shot vs chunked conversion must be byte-identical (continuous filter
    // and dither state across process() calls).
    let dsd = make_tone(1_000.0, 0.4, 200_000, DsdRate::Dsd64.hz() as f64);
    let spec = DsdSpec { rate: DsdRate::Dsd64, channels: 1, msb_first: true };

    let mut whole = Vec::new();
    DsdToPcm::new(spec, TargetRate::Pcm88_2)
        .unwrap()
        .process(&dsd, &mut whole);

    for cs in [1usize, 3, 7, 64, 333, 4096] {
        let mut conv = DsdToPcm::new(spec, TargetRate::Pcm88_2).unwrap();
        let mut chunked = Vec::new();
        for c in dsd.chunks(cs) {
            conv.process(c, &mut chunked);
        }
        assert_eq!(chunked, whole, "mismatch at chunk size {cs}");
    }
}

#[test]
fn reset_clears_state() {
    let spec = DsdSpec { rate: DsdRate::Dsd64, channels: 1, msb_first: true };
    let dsd = make_tone(1_000.0, 0.4, 80_000, DsdRate::Dsd64.hz() as f64);

    let mut a = Vec::new();
    let mut conv = DsdToPcm::new(spec, TargetRate::Pcm88_2).unwrap();
    conv.process(&dsd, &mut a);

    // Feed garbage, reset, then the same input -> identical to a fresh run.
    let mut junk = Vec::new();
    conv.process(&[0xAA, 0x55, 0x12], &mut junk);
    conv.reset();
    let mut b = Vec::new();
    conv.process(&dsd, &mut b);
    assert_eq!(a, b);
}
