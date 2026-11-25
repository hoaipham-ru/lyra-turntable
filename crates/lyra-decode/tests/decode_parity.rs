//! P1.1 — native-PCM decode parity against the golden `raw*.pcm` references,
//! exact `total_frames`, reported spec, and ADR-004 container selection.

mod common;

use std::fs;

use common::{data, decode_all, decode_all_opts};
use lyra_core::{SampleFormat, StreamSpec};
use lyra_decode::{open, probe, OpenOptions};

/// Every lossless fixture decodes byte-identically to its golden `raw` reference
/// (the same bytes `flac -d` / `ffmpeg pcm_s24le` produce), with the exact frame
/// count stated by the container.
#[test]
fn decode_matches_golden_raw() {
    // (file, golden raw, expected frames, rate, channels, bits_valid, format)
    let cases: &[(&str, &str, u64, u32, u16, u8, SampleFormat)] = &[
        ("flac16.flac", "raw16.pcm", 2205, 44_100, 2, 16, SampleFormat::S16),
        ("wav16.wav", "raw16.pcm", 2205, 44_100, 2, 16, SampleFormat::S16),
        ("flac24.flac", "raw24.pcm", 4800, 96_000, 2, 24, SampleFormat::S24),
        ("wav24.wav", "raw24.pcm", 4800, 96_000, 2, 24, SampleFormat::S24),
        ("flac51.flac", "raw51.pcm", 2205, 44_100, 6, 16, SampleFormat::S16),
        ("wav51.wav", "raw51.pcm", 2205, 44_100, 6, 16, SampleFormat::S16),
    ];

    for &(file, raw, frames, rate, ch, bits, fmt) in cases {
        let path = data(file);
        let golden = fs::read(data(raw)).expect("read golden raw");

        // probe() states the exact length and spec without decoding audio.
        let info = probe(&path).expect("probe");
        assert_eq!(info.total_frames, Some(frames), "{file}: probe total_frames");
        match info.spec {
            StreamSpec::Pcm(p) => {
                assert_eq!(p.rate, rate, "{file}: rate");
                assert_eq!(p.channels, ch, "{file}: channels");
                assert_eq!(p.bits_valid, bits, "{file}: bits_valid");
                assert_eq!(p.format, fmt, "{file}: format");
            }
            StreamSpec::Dsd(_) => panic!("{file}: unexpected DSD"),
        }

        let dec = open(&path, &OpenOptions::default()).expect("open");
        assert_eq!(dec.total_frames(), Some(frames), "{file}: total_frames()");
        drop(dec);

        let (bytes, read_frames) = decode_all(&path);
        assert_eq!(read_frames, frames, "{file}: frames read");
        assert_eq!(bytes.len(), golden.len(), "{file}: decoded byte length");
        assert!(bytes == golden, "{file}: decoded bytes != golden");
    }
}

/// FLAC and the equivalent WAV of the same source decode to identical bytes.
#[test]
fn flac_equals_wav() {
    for (f, w) in [("flac16.flac", "wav16.wav"), ("flac24.flac", "wav24.wav"), ("flac51.flac", "wav51.wav")] {
        let (fa, _) = decode_all(&data(f));
        let (wa, _) = decode_all(&data(w));
        assert_eq!(fa, wa, "{f} vs {w}");
    }
}

/// With `Container24::LeftJustifiedS32`, a 24-bit source is carried in a 32-bit
/// word, left-justified, `bits_valid` still 24, and the stored value is the packed
/// value `<< 8`.
#[test]
fn container24_left_justified_s32() {
    let packed = fs::read(data("raw24.pcm")).expect("golden");
    let opts = OpenOptions::default().with_24bit_in_s32();
    let (wide, frames) = decode_all_opts(&data("flac24.flac"), &opts);

    let dec = open(&data("flac24.flac"), &opts).expect("open");
    match dec.spec() {
        StreamSpec::Pcm(p) => {
            assert_eq!(p.format, SampleFormat::S32);
            assert_eq!(p.bits_valid, 24);
        }
        StreamSpec::Dsd(_) => unreachable!(),
    }
    drop(dec);

    // 4800 frames * 2 ch = 9600 samples; packed is 3 bytes/sample, wide 4.
    let samples = frames as usize * 2;
    assert_eq!(wide.len(), samples * 4);
    assert_eq!(packed.len(), samples * 3);

    for i in 0..samples {
        let p = &packed[i * 3..i * 3 + 3];
        let native = i32::from_le_bytes([p[0], p[1], p[2], if p[2] & 0x80 != 0 { 0xFF } else { 0 }]);
        let w = &wide[i * 4..i * 4 + 4];
        let stored = i32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        assert_eq!(stored, native << 8, "sample {i}: left-justified value");
    }
}

/// `read` appends to the caller's buffer and does not reallocate it when the
/// buffer already has capacity for the block — the per-`read` no-allocation rule
/// (the decoder owns and reuses its internal buffers; the output buffer is the
/// caller's and here it is pre-sized).
#[test]
fn read_reuses_caller_buffer_without_realloc() {
    let mut dec = open(&data("flac24.flac"), &OpenOptions::default()).expect("open");
    // One block is 4800 frames * 2 ch * 3 bytes = 28_800 bytes; reserve generously.
    let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
    let cap = out.capacity();
    let mut reads = 0;
    loop {
        out.clear(); // keeps capacity
        let n = dec.read(&mut out).expect("read");
        if n == 0 {
            break;
        }
        reads += 1;
        assert_eq!(out.capacity(), cap, "read reallocated the caller buffer");
    }
    assert!(reads >= 1, "expected at least one decoded block");
}

/// A non-audio file is rejected with an error, not a panic.
#[test]
fn rejects_non_audio() {
    let junk = common::scratch_dir().join("not_audio.flac");
    fs::write(&junk, b"this is not a flac file, just text padding bytes....").unwrap();
    let r = open(&junk, &OpenOptions::default());
    assert!(r.is_err(), "garbage file must error");
}
