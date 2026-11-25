//! P1.3 — sample-accurate `seek()` for FLAC and WAV: after `seek(n)` the next
//! `read` delivers frames starting exactly at frame `n`, matching the golden slice.

mod common;

use common::{data, decode_all};
use lyra_core::{Decoder, StreamSpec};
use lyra_decode::{open, OpenOptions};

fn bytes_per_frame(dec: &dyn Decoder) -> usize {
    match dec.spec() {
        StreamSpec::Pcm(p) => p.format.bytes_per_sample() * p.channels as usize,
        StreamSpec::Dsd(_) => unreachable!("PCM only in P1"),
    }
}

/// Read exactly `want` frames starting from the current position (after a seek),
/// returning the interleaved bytes actually produced (may be short at EOF).
fn read_frames(dec: &mut dyn Decoder, want: usize) -> Vec<u8> {
    let bpf = bytes_per_frame(dec);
    let mut out = Vec::new();
    let mut have = 0usize;
    while have < want {
        let before = out.len();
        let n = dec.read(&mut out).expect("read after seek");
        if n == 0 {
            break;
        }
        have += n;
        let _ = before;
    }
    out.truncate(want * bpf);
    out
}

/// 50 seek points across each fixture; the slice read after each seek equals the
/// corresponding slice of the full golden decode, byte for byte.
#[test]
fn seek_is_sample_accurate() {
    let fixtures = [
        ("flac16.flac", 2205usize),
        ("flac24.flac", 4800),
        ("flac51.flac", 2205),
        ("wav16.wav", 2205),
        ("wav24.wav", 4800),
        ("wav51.wav", 2205),
    ];

    for (file, total) in fixtures {
        let path = data(file);
        let (full, read_frames_total) = decode_all(&path);
        assert_eq!(read_frames_total as usize, total, "{file}: frame count");

        let mut dec = open(&path, &OpenOptions::default()).expect("open");
        let bpf = bytes_per_frame(dec.as_ref());

        // Deterministic spread of 50 seek points over [0, total), plus the edges.
        let mut points: Vec<u64> = (0..50).map(|i| (i * total / 50) as u64).collect();
        points.push(0);
        points.push((total - 1) as u64);
        points.push((total / 3) as u64);
        points.push((2 * total / 3) as u64);

        for &p in &points {
            let landed = dec.seek(p).expect("seek");
            assert_eq!(landed, p, "{file}: seek({p}) reported landing");

            let want = 128.min(total - p as usize);
            let got = read_frames(dec.as_mut(), want);
            let start = p as usize * bpf;
            let end = start + want * bpf;
            assert_eq!(
                got,
                &full[start..end],
                "{file}: slice after seek({p}) differs from golden"
            );
        }
    }
}

/// Seeking to `total_frames` lands exactly at end: the next read is EOF.
#[test]
fn seek_to_end_is_eof() {
    for (file, total) in [("flac16.flac", 2205u64), ("wav24.wav", 4800), ("flac51.flac", 2205)] {
        let mut dec = open(&data(file), &OpenOptions::default()).expect("open");
        let landed = dec.seek(total).expect("seek to end");
        assert_eq!(landed, total, "{file}: seek-to-end landing");
        let mut out = Vec::new();
        assert_eq!(dec.read(&mut out).expect("read at eof"), 0, "{file}: read past end");
        assert!(out.is_empty());
    }
}

/// Seeking past the end is rejected, not a panic.
#[test]
fn seek_past_end_errors() {
    let mut dec = open(&data("flac16.flac"), &OpenOptions::default()).expect("open");
    let r = dec.seek(2206);
    assert!(r.is_err(), "seek past total must error");
}

/// Re-seeking backward after reading forward still lands accurately.
#[test]
fn seek_backward_after_forward() {
    let path = data("flac24.flac");
    let (full, _) = decode_all(&path);
    let mut dec = open(&path, &OpenOptions::default()).expect("open");
    let bpf = bytes_per_frame(dec.as_ref());

    // Read some, seek forward, then seek back before the earlier read.
    let _ = read_frames(dec.as_mut(), 500);
    dec.seek(4000).expect("seek fwd");
    let _ = read_frames(dec.as_mut(), 100);
    dec.seek(200).expect("seek back");
    let got = read_frames(dec.as_mut(), 256);
    let start = 200 * bpf;
    assert_eq!(got, &full[start..start + 256 * bpf], "backward seek slice");
}
