//! P1.2 — libFLAC cross-check (feature `libflac-check`).
//!
//! ADR-007: FLAC is cross-checked against libFLAC — the decoder foobar2000 ships —
//! and if symphonia ever disagrees, libFLAC wins. This suite decodes representative
//! corpus FLACs (16-bit and 24-bit, several rates) with the symphonia path and
//! compares the bytes against `flac -d` via [`lyra_decode::libflac`]. The whole
//! file compiles to nothing unless the feature is on.
//!
//! The corpus is read-only (ADR-006): these tests only *read* corpus files and run
//! `flac -d` into the OS temp dir.
#![cfg(feature = "libflac-check")]

use std::path::{Path, PathBuf};

use lyra_core::StreamSpec;
use lyra_decode::{libflac, open, OpenOptions};

const CORPUS: &str = "C:\\SoulSeek_Music\\complete";

/// Walk the corpus and return the first FLAC of each requested (bits, rate-class).
/// Rate-class: `Lo` = 44.1/48 kHz, `Hi` = >= 88.2 kHz. Returns up to one per key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Key {
    Bits16,
    Bits24Lo,
    Bits24Hi,
}

fn classify(bits: u8, rate: u32) -> Option<Key> {
    match (bits, rate) {
        (16, _) => Some(Key::Bits16),
        (24, r) if r <= 48_000 => Some(Key::Bits24Lo),
        (24, _) => Some(Key::Bits24Hi),
        _ => None,
    }
}

fn collect_representatives() -> Vec<(Key, PathBuf)> {
    use std::collections::HashMap;
    let mut found: HashMap<Key, PathBuf> = HashMap::new();
    let root = Path::new(CORPUS);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            if found.len() == 3 {
                break;
            }
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("flac") {
                if let Ok(info) = lyra_decode::probe(&p) {
                    if let StreamSpec::Pcm(spec) = info.spec {
                        if let Some(k) = classify(spec.bits_valid, spec.rate) {
                            found.entry(k).or_insert(p);
                        }
                    }
                }
            }
        }
        if found.len() == 3 {
            break;
        }
    }
    found.into_iter().collect()
}

fn decode_default(path: &Path) -> (Vec<u8>, usize) {
    let mut dec = open(path, &OpenOptions::default()).expect("open");
    let bpf = match dec.spec() {
        StreamSpec::Pcm(p) => p.format.bytes_per_sample() * p.channels as usize,
        StreamSpec::Dsd(_) => unreachable!(),
    };
    let mut out = Vec::new();
    while dec.read(&mut out).expect("read") != 0 {}
    (out, bpf)
}

#[test]
fn symphonia_agrees_with_libflac_on_corpus() {
    if libflac::flac_binary().is_none() {
        eprintln!("flac CLI not found (PATH or winget); skipping libFLAC cross-check");
        return;
    }
    let reps = collect_representatives();
    if reps.is_empty() {
        eprintln!("no corpus FLACs found at {CORPUS}; skipping");
        return;
    }

    let mut covered16 = false;
    let mut covered24 = false;
    for (key, path) in &reps {
        let (ours, bpf) = decode_default(path);
        // libFLAC is authoritative: this returns Err naming the first differing
        // byte and frame if symphonia disagrees.
        libflac::cross_check(path, &ours, bpf)
            .unwrap_or_else(|e| panic!("{key:?} {}: {e}", path.display()));
        eprintln!("cross-check OK [{key:?}] {}", path.display());
        match key {
            Key::Bits16 => covered16 = true,
            Key::Bits24Lo | Key::Bits24Hi => covered24 = true,
        }
    }

    assert!(covered16, "expected at least one 16-bit corpus FLAC cross-checked");
    assert!(covered24, "expected at least one 24-bit corpus FLAC cross-checked");
}
