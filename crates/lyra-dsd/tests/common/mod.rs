//! Shared helpers for the lyra-dsd integration tests.

#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

/// A DSDIFF chunk: id + 64-bit BE size + data, padded to an even boundary.
pub fn dff_chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(id);
    v.extend_from_slice(&(data.len() as u64).to_be_bytes());
    v.extend_from_slice(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}

/// Build a tiny uncompressed stereo DSD64 DFF from per-channel byte vectors
/// (native MSB-first).
pub fn build_dff(l: &[u8], r: &[u8]) -> Vec<u8> {
    assert_eq!(l.len(), r.len());
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
    body.extend_from_slice(b"DSD "); // FRM8 form type
    body.extend(dff_chunk(b"FVER", &0x0105_0000u32.to_be_bytes()));
    body.extend(dff_chunk(b"PROP", &snd));
    body.extend(dff_chunk(b"DSD ", &sound));

    let mut file = Vec::new();
    file.extend_from_slice(b"FRM8");
    file.extend_from_slice(&(body.len() as u64).to_be_bytes());
    file.extend(body);
    file
}

/// Write `bytes` to a uniquely named file under the integration-test temp dir.
pub fn write_tmp(name: &str, bytes: &[u8]) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(name);
    fs::write(&p, bytes).unwrap();
    p
}

/// A unique path (not written) under the integration-test temp dir.
pub fn tmp_path(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(name);
    p
}
