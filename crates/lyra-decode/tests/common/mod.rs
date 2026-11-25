//! Shared helpers for the `lyra-decode` integration tests.
//!
//! The fixtures under `tests/data/` are tiny synthetic files (a few KB each):
//! * `flac16` / `wav16` — 44.1 kHz, 2 ch, 16-bit, 2205 frames.
//! * `flac24` / `wav24` — 96 kHz, 2 ch, 24-bit, 4800 frames.
//! * `flac51` / `wav51` — 44.1 kHz, 6 ch, 16-bit, 2205 frames.
//! * `rawNN.pcm` — the golden decoded output of the matching file in the default
//!   container (S16 for 16-bit, packed S24 for 24-bit), little-endian interleaved.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use lyra_decode::{open, OpenOptions};

/// Absolute path to a file under `tests/data/`.
pub fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join(name)
}

/// A writable scratch directory under `target/corpus-tmp/` (never the corpus,
/// ADR-006). Created if missing.
pub fn scratch_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../crates/lyra-decode ; workspace target is ../../target.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("corpus-tmp");
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Decode an entire file to interleaved native-PCM bytes with the default options,
/// returning `(bytes, total_frames_read)`.
pub fn decode_all(path: &Path) -> (Vec<u8>, u64) {
    decode_all_opts(path, &OpenOptions::default())
}

/// Decode an entire file to interleaved native-PCM bytes with the given options.
pub fn decode_all_opts(path: &Path, opts: &OpenOptions) -> (Vec<u8>, u64) {
    let mut dec = open(path, opts).expect("open decoder");
    let mut out = Vec::new();
    let mut frames = 0u64;
    loop {
        let before = out.len();
        let n = dec.read(&mut out).expect("read");
        if n == 0 {
            break;
        }
        frames += n as u64;
        // The decoder must append exactly `n` frames' worth of bytes.
        let bpf = dec.spec().channels() as usize
            * match dec.spec() {
                lyra_core::StreamSpec::Pcm(p) => p.format.bytes_per_sample(),
                lyra_core::StreamSpec::Dsd(_) => unreachable!("PCM only in P1"),
            };
        assert_eq!(out.len() - before, n * bpf, "read appended wrong byte count");
    }
    (out, frames)
}
