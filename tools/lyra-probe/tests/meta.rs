//! End-to-end tests of `lyra-probe meta` (P6.5), driving the built binary.
//!
//! Coverage:
//! * a synthetic PCM WAV (always runs): every §6 field is emitted and
//!   `length_samples` equals the decoder's exact frame count.
//! * a synthetic DSD64 DFF (always runs): `read_tags` cannot parse it, so `meta`
//!   must fall back to the `lyra-dsd` reader — codec `DSD`, 1-bit, the DSD rate in
//!   Hz, and `length_samples` == the reader's `total_frames`.
//! * a real corpus FLAC (skipped when the corpus is absent): all fields present,
//!   `length_samples` == the frames actually decoded by `lyra-decode`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The §6 JSON keys `meta --json` must always emit.
const SECTION6_KEYS: &[&str] = &[
    "title",
    "artist",
    "album",
    "album_artist",
    "tracknumber",
    "discnumber",
    "date",
    "genre",
    "length_samples",
    "samplerate",
    "bitspersample",
    "channels",
    "codec",
    "replaygain_track_gain",
    "replaygain_album_gain",
];

fn tmp(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(name);
    p
}

/// Run `meta <input> --json` and parse stdout as JSON, asserting a clean exit.
fn run_meta_json(input: &Path) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_lyra-probe"))
        .arg("meta")
        .arg(input)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "meta should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("meta --json should emit valid JSON")
}

fn assert_all_section6_keys(v: &Value) {
    let obj = v.as_object().expect("meta JSON must be an object");
    for k in SECTION6_KEYS {
        assert!(obj.contains_key(*k), "§6 key `{k}` missing from meta output");
    }
}

/// A canonical 44-byte-header PCM WAV: 16-bit stereo at `rate`, `frames` frames.
fn build_wav(rate: u32, frames: u32) -> Vec<u8> {
    let channels: u16 = 2;
    let bits: u16 = 16;
    let block_align = channels * bits / 8;
    let data_size = frames * u32::from(block_align);
    let byte_rate = rate * u32::from(block_align);

    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_size).to_le_bytes());
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&byte_rate.to_le_bytes());
    v.extend_from_slice(&block_align.to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_size.to_le_bytes());
    // A simple ramp so the payload is non-trivial.
    for i in 0..frames {
        let s = (i as i16).wrapping_mul(7);
        v.extend_from_slice(&s.to_le_bytes()); // L
        v.extend_from_slice(&s.to_le_bytes()); // R
    }
    v
}

fn dff_chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(id);
    v.extend_from_slice(&(data.len() as u64).to_be_bytes());
    v.extend_from_slice(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}

/// Minimal uncompressed stereo DSD64 DFF from per-channel bytes (MSB-first).
fn build_dff(l: &[u8], r: &[u8]) -> Vec<u8> {
    let mut sound = Vec::new();
    for i in 0..l.len() {
        sound.push(l[i]);
        sound.push(r[i]);
    }
    let mut snd = Vec::new();
    snd.extend_from_slice(b"SND ");
    snd.extend(dff_chunk(b"FS  ", &2_822_400u32.to_be_bytes()));
    let mut chnl = Vec::new();
    chnl.extend_from_slice(&2u16.to_be_bytes());
    chnl.extend_from_slice(b"SLFT");
    chnl.extend_from_slice(b"SRGT");
    snd.extend(dff_chunk(b"CHNL", &chnl));
    let mut cmpr = Vec::new();
    cmpr.extend_from_slice(b"DSD ");
    cmpr.push(14);
    cmpr.extend_from_slice(b"not compressed");
    snd.extend(dff_chunk(b"CMPR", &cmpr));

    let mut body = Vec::new();
    body.extend_from_slice(b"DSD ");
    body.extend(dff_chunk(b"FVER", &0x0105_0000u32.to_be_bytes()));
    body.extend(dff_chunk(b"PROP", &snd));
    body.extend(dff_chunk(b"DSD ", &sound));

    let mut file = Vec::new();
    file.extend_from_slice(b"FRM8");
    file.extend_from_slice(&(body.len() as u64).to_be_bytes());
    file.extend(body);
    file
}

/// The exact number of frames `lyra-decode` yields for a PCM file.
fn decoded_frame_count(path: &Path) -> u64 {
    let info = lyra_decode::probe(path).unwrap();
    let spec = match info.spec {
        lyra_core::StreamSpec::Pcm(p) => p,
        lyra_core::StreamSpec::Dsd(_) => panic!("expected PCM"),
    };
    let mut dec = lyra_decode::open(path, &lyra_decode::OpenOptions::default()).unwrap();
    let mut pcm = Vec::new();
    while dec.read(&mut pcm).unwrap() != 0 {}
    (pcm.len() / spec.bytes_per_frame()) as u64
}

#[test]
fn meta_on_synthetic_wav_emits_all_fields_and_exact_length() {
    let frames = 12_345u32;
    let path = tmp("meta.wav");
    fs::write(&path, build_wav(44_100, frames)).unwrap();

    let v = run_meta_json(&path);
    assert_all_section6_keys(&v);

    assert_eq!(v["samplerate"], Value::from(44_100));
    assert_eq!(v["bitspersample"], Value::from(16));
    assert_eq!(v["channels"], Value::from(2));
    assert_eq!(v["codec"], Value::from("PCM"));
    assert_eq!(v["_source"], Value::from("pcm"));

    // The exact length must equal what the decoder actually produces, not the
    // approximate tag-derived count.
    let exact = decoded_frame_count(&path);
    assert_eq!(exact, u64::from(frames), "sanity: decoded frame count");
    assert_eq!(
        v["length_samples"].as_u64(),
        Some(exact),
        "length_samples must equal the decoded frame count"
    );
    // Internal consistency: reported format == the decoder's own view.
    assert_eq!(v["samplerate"], v["_decoder"]["samplerate"]);
    assert_eq!(v["channels"], v["_decoder"]["channels"]);
    assert_eq!(v["bitspersample"], v["_decoder"]["bits"]);
}

#[test]
fn meta_on_synthetic_dff_falls_back_to_dsd_reader() {
    // 4096+500 DSD bytes/channel => 8*(4596) one-bit frames per channel.
    let n = 4096 + 500;
    let l: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(29).wrapping_add(1)).collect();
    let r: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(29).wrapping_add(99)).collect();
    let path = tmp("meta.dff");
    fs::write(&path, build_dff(&l, &r)).unwrap();

    let v = run_meta_json(&path);
    assert_all_section6_keys(&v);

    assert_eq!(v["codec"], Value::from("DSD"));
    assert_eq!(v["samplerate"], Value::from(2_822_400));
    assert_eq!(v["bitspersample"], Value::from(1));
    assert_eq!(v["channels"], Value::from(2));
    assert_eq!(v["_source"], Value::from("dsd"));

    let expected_frames = (n as u64) * 8;
    assert_eq!(
        v["length_samples"].as_u64(),
        Some(expected_frames),
        "DSD length_samples must equal total_frames from the lyra-dsd reader"
    );
    // Tag text columns are null for raw DSD (no ID3 parse here).
    assert!(v["title"].is_null());
    assert!(v["artist"].is_null());
}

#[test]
fn meta_on_corpus_flac_matches_decoded_length() {
    let Some(flac) = first_corpus_flac() else {
        eprintln!("skip: no corpus FLAC found (set LYRA_CORPUS or mount the corpus)");
        return;
    };
    eprintln!("corpus FLAC: {}", flac.display());

    let v = run_meta_json(&flac);
    assert_all_section6_keys(&v);
    assert_eq!(v["codec"], Value::from("FLAC"));
    assert_eq!(v["_source"], Value::from("pcm"));

    let exact = decoded_frame_count(&flac);
    assert_eq!(
        v["length_samples"].as_u64(),
        Some(exact),
        "length_samples must equal the frames lyra-decode produces for {}",
        flac.display()
    );
    // The §6 format columns must agree with the decoder's own view.
    assert_eq!(v["samplerate"], v["_decoder"]["samplerate"]);
    assert_eq!(v["channels"], v["_decoder"]["channels"]);
    assert_eq!(v["bitspersample"], v["_decoder"]["bits"]);
}

/// First `.flac` under the corpus (`LYRA_CORPUS` or the default read-only path),
/// or `None` so the test is skipped when the corpus is not present.
fn first_corpus_flac() -> Option<PathBuf> {
    let root = std::env::var_os("LYRA_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\SoulSeek_Music\complete"));
    if !root.is_dir() {
        return None;
    }
    find_flac(&root)
}

/// Depth-first search for the first `.flac` under `dir` (read-only).
fn find_flac(dir: &Path) -> Option<PathBuf> {
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let entries = fs::read_dir(&d).ok()?;
        let mut sub = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                sub.push(p);
            } else if p.extension().and_then(|x| x.to_str()).map(|x| x.eq_ignore_ascii_case("flac")) == Some(true) {
                return Some(p);
            }
        }
        dirs.extend(sub);
    }
    None
}
