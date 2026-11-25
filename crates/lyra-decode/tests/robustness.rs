//! P1.4 — metadata-block tolerance and malformed-audio robustness.
//!
//! A broken/oversized `PICTURE`, oversized `PADDING`, or a leading ID3v2 tag must
//! never stop the audio from decoding; the decoded PCM is unchanged. Corrupt
//! *audio* returns a `LyraError::Decode` carrying the frame position and never
//! panics. All corrupt copies are written under `target/corpus-tmp/` — never the
//! read-only corpus (ADR-006).

mod common;

use std::fs;
use std::path::Path;

use common::{data, decode_all, scratch_dir};
use lyra_core::LyraError;
use lyra_decode::{open, OpenOptions};

const FLAC_MARKER: &[u8; 4] = b"fLaC";

/// A parsed FLAC file split into its metadata blocks and audio frames.
struct FlacParts {
    /// `(block_type, body)` for every metadata block, STREAMINFO first.
    blocks: Vec<(u8, Vec<u8>)>,
    audio: Vec<u8>,
}

fn parse_flac(bytes: &[u8]) -> FlacParts {
    assert_eq!(&bytes[0..4], FLAC_MARKER, "fixture is not a FLAC");
    let mut pos = 4usize;
    let mut blocks = Vec::new();
    loop {
        let header = bytes[pos];
        let is_last = header & 0x80 != 0;
        let block_type = header & 0x7F;
        let len = (usize::from(bytes[pos + 1]) << 16)
            | (usize::from(bytes[pos + 2]) << 8)
            | usize::from(bytes[pos + 3]);
        let body = bytes[pos + 4..pos + 4 + len].to_vec();
        blocks.push((block_type, body));
        pos += 4 + len;
        if is_last {
            break;
        }
    }
    FlacParts { blocks, audio: bytes[pos..].to_vec() }
}

fn write_block(out: &mut Vec<u8>, block_type: u8, body: &[u8], is_last: bool) {
    let mut h = block_type & 0x7F;
    if is_last {
        h |= 0x80;
    }
    out.push(h);
    let len = body.len();
    out.push((len >> 16) as u8);
    out.push((len >> 8) as u8);
    out.push(len as u8);
    out.extend_from_slice(body);
}

/// Rebuild FLAC bytes from parts (last-block flag set on the final metadata block).
fn rebuild(parts: &FlacParts) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(FLAC_MARKER);
    for (i, (ty, body)) in parts.blocks.iter().enumerate() {
        write_block(&mut out, *ty, body, i + 1 == parts.blocks.len());
    }
    out.extend_from_slice(&parts.audio);
    out
}

fn write_scratch(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = scratch_dir().join(name);
    fs::write(&p, bytes).expect("write scratch");
    p
}

/// A PICTURE block body whose internal MIME-length field is absurd (0xFFFF_FFFF)
/// while the block header length is honest. symphonia chokes parsing the picture;
/// the tolerant path must drop it and still decode the audio.
fn broken_picture_body() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&3u32.to_be_bytes()); // picture type = front cover
    b.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // MIME length: bogus/huge
    b.extend_from_slice(b"junk"); // far fewer bytes than claimed
    b
}

/// Leading ID3v2 tag, declared size matching its body, before the FLAC stream.
fn id3v2_tag(payload: &[u8]) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"ID3");
    t.push(4); // version major
    t.push(0); // version minor
    t.push(0); // flags (no footer)
    let n = payload.len();
    // 28-bit sync-safe size, 7 bits per byte.
    t.push(((n >> 21) & 0x7F) as u8);
    t.push(((n >> 14) & 0x7F) as u8);
    t.push(((n >> 7) & 0x7F) as u8);
    t.push((n & 0x7F) as u8);
    t.extend_from_slice(payload);
    t
}

/// Outcome of a tolerant decode of a possibly-corrupt file.
enum Outcome {
    /// Decoded cleanly to `n` frames.
    Ok(u64),
    /// `open()` failed before any audio (e.g. truncated metadata). No frame yet.
    OpenErr(String),
    /// A read-time (audio) decode error after `frames_before` frames; the message
    /// is a `LyraError::Decode` carrying the frame position.
    ReadErr { frames_before: u64, msg: String },
}

/// Decode a file that may have corrupt audio; this must never panic. A read-time
/// error is required to be a `LyraError::Decode` whose message carries a frame
/// position; that invariant is enforced here.
fn decode_tolerant(path: &Path) -> Outcome {
    let mut dec = match open(path, &OpenOptions::default()) {
        Ok(d) => d,
        Err(e) => return Outcome::OpenErr(e.to_string()),
    };
    let mut out = Vec::new();
    let mut frames = 0u64;
    loop {
        out.clear();
        match dec.read(&mut out) {
            Ok(0) => break,
            Ok(n) => frames += n as u64,
            Err(e) => {
                let LyraError::Decode(msg) = &e else {
                    panic!("corrupt audio must be LyraError::Decode, got {e:?}");
                };
                assert!(
                    msg.contains("frame"),
                    "decode error must carry the frame position, got: {msg}"
                );
                return Outcome::ReadErr { frames_before: frames, msg: msg.clone() };
            }
        }
    }
    Outcome::Ok(frames)
}

/// First `.flac` found in the read-only corpus, or `None` if the corpus is absent.
fn any_corpus_flac() -> Option<std::path::PathBuf> {
    let root = Path::new("C:\\SoulSeek_Music\\complete");
    if !root.exists() {
        return None;
    }
    fn walk(dir: &Path) -> Option<std::path::PathBuf> {
        let mut entries: Vec<_> = fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(std::fs::DirEntry::path);
        for e in &entries {
            let p = e.path();
            if p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("flac") {
                return Some(p);
            }
        }
        for e in &entries {
            let p = e.path();
            if p.is_dir() {
                if let Some(f) = walk(&p) {
                    return Some(f);
                }
            }
        }
        None
    }
    walk(root)
}

/// The golden PCM of a clean fixture, for comparing tolerant decodes against.
fn clean_pcm(file: &str) -> Vec<u8> {
    decode_all(&data(file)).0
}

#[test]
fn broken_picture_still_decodes() {
    let clean = clean_pcm("flac16.flac");
    let mut parts = parse_flac(&fs::read(data("flac16.flac")).unwrap());
    // Insert the broken PICTURE right after STREAMINFO.
    parts.blocks.insert(1, (6, broken_picture_body()));
    let p = write_scratch("broken_picture.flac", &rebuild(&parts));

    let (pcm, _) = decode_all(&p);
    assert_eq!(pcm, clean, "broken-picture FLAC must decode to the same PCM");
}

#[test]
fn oversized_padding_still_decodes() {
    let clean = clean_pcm("flac16.flac");
    let mut parts = parse_flac(&fs::read(data("flac16.flac")).unwrap());
    // 256 KiB padding block (type 1) — well-formed but large.
    parts.blocks.push((1, vec![0u8; 256 * 1024]));
    let p = write_scratch("oversized_padding.flac", &rebuild(&parts));

    let (pcm, _) = decode_all(&p);
    assert_eq!(pcm, clean, "oversized-padding FLAC must decode to the same PCM");
}

#[test]
fn leading_id3_still_decodes() {
    let clean = clean_pcm("flac16.flac");
    let flac = fs::read(data("flac16.flac")).unwrap();
    let mut file = id3v2_tag(b"TXXX fake id3 payload for a FLAC that should be skipped");
    file.extend_from_slice(&flac);
    let p = write_scratch("leading_id3.flac", &file);

    let (pcm, _) = decode_all(&p);
    assert_eq!(pcm, clean, "ID3-prefixed FLAC must decode to the same PCM");
}

#[test]
fn broken_picture_plus_id3_still_decodes() {
    let clean = clean_pcm("flac16.flac");
    let mut parts = parse_flac(&fs::read(data("flac16.flac")).unwrap());
    parts.blocks.insert(1, (6, broken_picture_body()));
    let mut file = id3v2_tag(b"combo");
    file.extend_from_slice(&rebuild(&parts));
    let p = write_scratch("id3_and_broken_picture.flac", &file);

    let (pcm, _) = decode_all(&p);
    assert_eq!(pcm, clean, "ID3 + broken picture must decode to the same PCM");
}

#[test]
fn truncated_audio_no_panic() {
    let flac = fs::read(data("flac16.flac")).unwrap();
    // The fixture's single audio frame starts at offset 8288 (per `flac -a`); the
    // file is 8894 bytes. Cut inside the audio frame so metadata still parses.
    let cut = &flac[..8600];
    let p = write_scratch("truncated.flac", cut);

    // Must not panic. Truncated audio yields either a short/empty decode (symphonia
    // reports EOF mid-frame) or a frame-tagged Decode error; never a panic.
    match decode_tolerant(&p) {
        Outcome::Ok(frames) => assert!(frames <= 2205, "truncated frame count sane"),
        Outcome::ReadErr { .. } => { /* frame tag already asserted in decode_tolerant */ }
        Outcome::OpenErr(msg) => panic!("metadata should still parse, got open error: {msg}"),
    }
}

#[test]
fn flipped_byte_no_panic() {
    let mut flac = fs::read(data("flac16.flac")).unwrap();
    // Flip bytes well inside the audio region (audio starts at offset 8288).
    let at = 8400.min(flac.len() - 8);
    for i in 0..8 {
        flac[at + i] ^= 0xFF;
    }
    let p = write_scratch("flipped_byte.flac", &flac);

    // Must not panic. A CRC/decode failure must be a frame-tagged Decode error
    // (enforced inside decode_tolerant); silent tolerance is also acceptable here.
    match decode_tolerant(&p) {
        Outcome::Ok(_) | Outcome::ReadErr { .. } => {}
        Outcome::OpenErr(msg) => panic!("metadata should still parse, got open error: {msg}"),
    }
}

/// A real multi-frame corpus FLAC with bytes flipped in a middle audio frame must
/// not panic; metadata still opens, early frames decode, and any decode failure is
/// a `LyraError::Decode` that names the frame position *after* the frames already
/// produced. Copied to `target/corpus-tmp/` — the corpus is never written (ADR-006).
#[test]
fn corpus_midstream_corruption_no_panic() {
    let Some(src) = any_corpus_flac() else {
        eprintln!("corpus not present; skipping corpus_midstream_corruption_no_panic");
        return;
    };
    let mut bytes = fs::read(&src).expect("read corpus flac");
    assert!(bytes.len() > 100_000, "corpus file unexpectedly tiny");

    // Flip 64 bytes around the 55% mark — well past the metadata, inside audio.
    let at = bytes.len() * 55 / 100;
    for b in &mut bytes[at..at + 64] {
        *b ^= 0xA5;
    }
    let p = write_scratch("corpus_midstream_corrupt.flac", &bytes);

    match decode_tolerant(&p) {
        Outcome::Ok(frames) => {
            // symphonia may resync and finish; just prove it produced audio.
            assert!(frames > 0, "expected some decoded frames");
        }
        Outcome::ReadErr { frames_before, msg } => {
            // The error must name a frame, and some frames must have decoded first.
            assert!(frames_before > 0, "corruption is mid-stream; earlier frames decode");
            assert!(msg.contains("frame"), "error carries frame position: {msg}");
        }
        Outcome::OpenErr(msg) => panic!("metadata should still open, got: {msg}"),
    }
}

#[test]
fn empty_and_marker_only_error_cleanly() {
    let empty = write_scratch("empty.flac", b"");
    assert!(open(&empty, &OpenOptions::default()).is_err());

    let marker = write_scratch("marker_only.flac", FLAC_MARKER);
    assert!(open(&marker, &OpenOptions::default()).is_err());
}
