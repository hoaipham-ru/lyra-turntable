//! DoP packer golden vectors and chunk-size invariance (S03).

use lyra_core::{DsdRate, DsdSpec};
use lyra_dsd::DopPacker;

fn spec(channels: u16, msb_first: bool) -> DsdSpec {
    DsdSpec { rate: DsdRate::Dsd64, channels, msb_first }
}

#[test]
fn mono_msb_first_golden() {
    // Two PCM frames (2 DSD bytes each). Marker alternates 0x05, 0xFA.
    let mut p = DopPacker::new(spec(1, true));
    let mut out = Vec::new();
    p.pack(&[0x01, 0x02, 0x03, 0x04], &mut out);
    assert_eq!(
        out,
        vec![
            // frame 0: value 0x05_01_02 -> LE bytes
            0x02, 0x01, 0x05, //
            // frame 1: value 0xFA_03_04 -> LE bytes
            0x04, 0x03, 0xFA,
        ]
    );
}

#[test]
fn stereo_msb_first_golden() {
    // Interleaved input A_ch0 A_ch1 B_ch0 B_ch1, one PCM frame.
    let mut p = DopPacker::new(spec(2, true));
    let mut out = Vec::new();
    p.pack(&[0x11, 0x21, 0x12, 0x22], &mut out);
    assert_eq!(
        out,
        vec![
            // ch0: 0x05_11_12
            0x12, 0x11, 0x05, //
            // ch1: 0x05_21_22
            0x22, 0x21, 0x05,
        ]
    );
}

#[test]
fn lsb_first_is_bit_reversed_in_word() {
    // Same input bytes but source LSB-first: each DSD byte is bit-reversed so the
    // DAC gets MSB-first DSD regardless of container. This test catches a swapped
    // bit order.
    let mut p = DopPacker::new(spec(1, false));
    let mut out = Vec::new();
    p.pack(&[0x01, 0x02], &mut out);
    // reverse_bits(0x01)=0x80, reverse_bits(0x02)=0x40 -> 0x05_80_40
    assert_eq!(out, vec![0x40, 0x80, 0x05]);
}

#[test]
fn marker_continuous_across_calls() {
    let mut p = DopPacker::new(spec(1, true));
    let mut out = Vec::new();
    p.pack(&[0x00, 0x00], &mut out); // frame 0 -> 0x05
    p.pack(&[0x00, 0x00], &mut out); // frame 1 -> 0xFA
    p.pack(&[0x00, 0x00], &mut out); // frame 2 -> 0x05
    assert_eq!(out[2], 0x05);
    assert_eq!(out[5], 0xFA);
    assert_eq!(out[8], 0x05);
}

#[test]
fn reset_restarts_marker() {
    let mut p = DopPacker::new(spec(1, true));
    let mut out = Vec::new();
    p.pack(&[0, 0, 0, 0], &mut out); // frames 0,1 -> 0x05,0xFA
    assert_eq!(out[2], 0x05);
    assert_eq!(out[5], 0xFA);
    out.clear();
    p.reset();
    p.pack(&[0, 0], &mut out); // frame 0 again -> 0x05
    assert_eq!(out[2], 0x05);
}

#[test]
fn out_spec_matches_dop_carrier_rate() {
    let p = DopPacker::new(spec(2, true));
    let s = p.out_spec();
    assert_eq!(s.rate, DsdRate::Dsd64.dop_pcm_rate()); // 176_400
    assert_eq!(s.channels, 2);
    assert_eq!(s.bits_valid, 24);
}

/// Build a deterministic pseudo-random DSD byte stream.
fn stream(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..len {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        v.push((x >> 24) as u8);
    }
    v
}

#[test]
fn chunking_invariant_every_size_1_to_4096() {
    // Stereo stream, length not a multiple of the 4-byte frame so partial frames
    // are exercised. One-shot output is the reference.
    let data = stream(4000);
    let sp = spec(2, true);

    let mut reference = Vec::new();
    DopPacker::new(sp).pack(&data, &mut reference);

    for cs in 1..=4096usize {
        let mut p = DopPacker::new(sp);
        let mut out = Vec::new();
        for chunk in data.chunks(cs) {
            p.pack(chunk, &mut out);
        }
        assert_eq!(out, reference, "mismatch at chunk size {cs}");
    }
}

#[test]
fn chunking_invariant_lsb_first() {
    // Same invariant with an LSB-first source (bit-reversal in play).
    let data = stream(2048);
    let sp = spec(2, false);

    let mut reference = Vec::new();
    DopPacker::new(sp).pack(&data, &mut reference);

    for cs in 1..=4096usize {
        let mut p = DopPacker::new(sp);
        let mut out = Vec::new();
        for chunk in data.chunks(cs) {
            p.pack(chunk, &mut out);
        }
        assert_eq!(out, reference, "mismatch at chunk size {cs}");
    }
}
