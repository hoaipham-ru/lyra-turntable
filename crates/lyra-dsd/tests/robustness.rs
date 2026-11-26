//! Negative tests: malformed DSF/DFF must return a clean `LyraError`, never panic
//! or hang. These run under the default (overflow-checks-on) test profile, which
//! is where the B1 chunk-size overflow used to abort.

use std::fs;
use std::path::PathBuf;

use lyra_core::LyraError;
use lyra_dsd::probe;

// --- helpers ------------------------------------------------------------------

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

/// A valid uncompressed stereo DSD64 DFF with `n` bytes per channel.
fn valid_dff(n: usize) -> Vec<u8> {
    let mut sound = Vec::new();
    for i in 0..n {
        sound.push((i as u8).wrapping_mul(7).wrapping_add(1)); // L
        sound.push((i as u8).wrapping_mul(7).wrapping_add(2)); // R
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

/// A valid stereo DSD64 DSF, block size 8, `n` bytes/channel.
fn valid_dsf(n: usize) -> Vec<u8> {
    let block = 8usize;
    let blocks = n.div_ceil(block);
    let mut data = Vec::new();
    for g in 0..blocks {
        for which in 0..2u8 {
            for j in 0..block {
                let idx = g * block + j;
                data.push(if idx < n {
                    (idx as u8).wrapping_mul(5).wrapping_add(which)
                } else {
                    0
                });
            }
        }
    }
    let sample_count = (n as u64) * 8;
    let data_chunk_size = data.len() as u64 + 12;

    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u32.to_le_bytes());
    fmt.extend_from_slice(&0u32.to_le_bytes());
    fmt.extend_from_slice(&2u32.to_le_bytes());
    fmt.extend_from_slice(&2u32.to_le_bytes());
    fmt.extend_from_slice(&2_822_400u32.to_le_bytes());
    fmt.extend_from_slice(&1u32.to_le_bytes());
    fmt.extend_from_slice(&sample_count.to_le_bytes());
    fmt.extend_from_slice(&(block as u32).to_le_bytes());
    fmt.extend_from_slice(&0u32.to_le_bytes());

    let total_file_size = 28u64 + 12 + fmt.len() as u64 + data_chunk_size;
    let mut file = Vec::new();
    file.extend_from_slice(b"DSD ");
    file.extend_from_slice(&28u64.to_le_bytes());
    file.extend_from_slice(&total_file_size.to_le_bytes());
    file.extend_from_slice(&0u64.to_le_bytes());
    file.extend_from_slice(b"fmt ");
    file.extend_from_slice(&(12u64 + fmt.len() as u64).to_le_bytes());
    file.extend_from_slice(&fmt);
    file.extend_from_slice(b"data");
    file.extend_from_slice(&data_chunk_size.to_le_bytes());
    file.extend_from_slice(&data);
    file
}

fn write_tmp(name: &str, bytes: &[u8]) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(name);
    fs::write(&p, bytes).unwrap();
    p
}

/// SOUND_COUNT field offset inside a `valid_dsf` file: DSD(28) + "fmt "(4) +
/// size(8) + body offset 36.
const DSF_SAMPLE_COUNT_OFFSET: usize = 28 + 4 + 8 + 36;

// --- DFF ----------------------------------------------------------------------

#[test]
fn dff_chunk_size_near_u64_max_does_not_panic() {
    // B1 reproducer: a 28-byte DFF whose single chunk claims size u64::MAX.
    let mut f = Vec::new();
    f.extend_from_slice(b"FRM8");
    f.extend_from_slice(&16u64.to_be_bytes()); // form size -> inner_end covers chunk
    f.extend_from_slice(b"DSD "); // form type
    f.extend_from_slice(b"DSD "); // chunk id (sound)
    f.extend_from_slice(&u64::MAX.to_be_bytes()); // chunk size -> would overflow
    assert_eq!(f.len(), 28);

    let path = write_tmp("overflow.dff", &f);
    match probe(&path) {
        Err(LyraError::Decode(_)) => {}
        other => panic!("expected Decode error, got {other:?}"),
    }
}

#[test]
fn dff_truncated_sound_chunk_errors() {
    // A valid DFF truncated inside its sound data: the DSD chunk's declared size
    // now runs past the (shortened) file.
    let full = valid_dff(64);
    let truncated = &full[..full.len() - 80];
    let path = write_tmp("truncated.dff", truncated);
    match probe(&path) {
        Err(LyraError::Decode(_)) => {}
        other => panic!("expected Decode error, got {other:?}"),
    }
}

#[test]
fn dff_prop_size_overflow_errors() {
    // A PROP chunk claiming size near u64::MAX must be rejected, not panic.
    let mut f = Vec::new();
    f.extend_from_slice(b"FRM8");
    f.extend_from_slice(&16u64.to_be_bytes());
    f.extend_from_slice(b"DSD ");
    f.extend_from_slice(b"PROP");
    f.extend_from_slice(&(u64::MAX - 1).to_be_bytes());
    let path = write_tmp("prop_overflow.dff", &f);
    match probe(&path) {
        Err(LyraError::Decode(_)) => {}
        other => panic!("expected Decode error, got {other:?}"),
    }
}

#[test]
fn dff_still_parses_when_valid() {
    // The tightened bounds must not reject a well-formed file.
    let path = write_tmp("ok.dff", &valid_dff(40));
    let info = probe(&path).unwrap();
    assert_eq!(info.total_frames, 40 * 8);
    assert_eq!(info.spec.channels, 2);
}

// --- DSF ----------------------------------------------------------------------

#[test]
fn dsf_sample_count_near_u64_max_does_not_panic() {
    let mut f = valid_dsf(40);
    f[DSF_SAMPLE_COUNT_OFFSET..DSF_SAMPLE_COUNT_OFFSET + 8]
        .copy_from_slice(&u64::MAX.to_le_bytes());
    let path = write_tmp("dsf_huge_count.dsf", &f);
    match probe(&path) {
        Err(LyraError::Decode(_)) => {}
        other => panic!("expected Decode error, got {other:?}"),
    }
}

#[test]
fn dsf_sample_count_exceeds_data_errors() {
    // Claim a few more samples than the data chunk can hold.
    let mut f = valid_dsf(40);
    let inflated = (40u64 * 8) + 8 * 100; // 100 bytes/channel too many
    f[DSF_SAMPLE_COUNT_OFFSET..DSF_SAMPLE_COUNT_OFFSET + 8]
        .copy_from_slice(&inflated.to_le_bytes());
    let path = write_tmp("dsf_overclaim.dsf", &f);
    match probe(&path) {
        Err(LyraError::Decode(_)) => {}
        other => panic!("expected Decode error, got {other:?}"),
    }
}

#[test]
fn dsf_truncated_data_errors() {
    // A valid DSF truncated inside its data: header sample_count now claims more
    // than the file holds.
    let full = valid_dsf(64);
    let truncated = &full[..full.len() - 40];
    let path = write_tmp("dsf_truncated.dsf", truncated);
    match probe(&path) {
        Err(LyraError::Decode(_)) => {}
        other => panic!("expected Decode error, got {other:?}"),
    }
}

#[test]
fn dsf_still_parses_when_valid() {
    let path = write_tmp("ok.dsf", &valid_dsf(40));
    let info = probe(&path).unwrap();
    assert_eq!(info.total_frames, 40 * 8);
    assert_eq!(info.spec.channels, 2);
}
