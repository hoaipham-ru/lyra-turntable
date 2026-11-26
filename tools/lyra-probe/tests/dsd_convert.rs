//! End-to-end test of `lyra-probe dsd convert` (P5.3), driving the built binary.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use lyra_core::{Decoder, StreamSpec};

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

fn tmp(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    p.push(name);
    p
}

fn read_all(dec: &mut dyn Decoder) -> Vec<u8> {
    let mut out = Vec::new();
    while dec.read(&mut out).unwrap() != 0 {}
    out
}

#[test]
fn convert_dff_to_dsf_via_cli() {
    let n = 4096 + 500;
    let l: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(29).wrapping_add(1)).collect();
    let r: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(29).wrapping_add(99)).collect();

    let dff_path = tmp("cli.dff");
    fs::write(&dff_path, build_dff(&l, &r)).unwrap();
    // CARGO_TARGET_TMPDIR lives under the crate's target/ dir, satisfying ADR-006.
    let dsf_path = tmp("cli.dsf");

    let status = Command::new(env!("CARGO_BIN_EXE_lyra-probe"))
        .arg("dsd")
        .arg("convert")
        .arg(&dff_path)
        .arg(&dsf_path)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0), "convert should exit 0");
    assert!(dsf_path.exists());

    // Decode both and confirm identical DSD after bit-order normalisation.
    let mut dff = lyra_dsd::open(&dff_path).unwrap();
    let mut dsf = lyra_dsd::open(&dsf_path).unwrap();
    assert!(matches!(dsf.spec(), StreamSpec::Dsd(s) if !s.msb_first));
    let dff_out = read_all(dff.as_mut());
    let dsf_norm: Vec<u8> = read_all(dsf.as_mut()).iter().map(|b| b.reverse_bits()).collect();
    assert_eq!(dsf_norm, dff_out);
}

#[test]
fn refuses_write_outside_target() {
    let n = 16;
    let l: Vec<u8> = (0..n).map(|i| i as u8).collect();
    let r: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_add(50)).collect();
    let dff_path = tmp("guard.dff");
    fs::write(&dff_path, build_dff(&l, &r)).unwrap();

    // A path with no `target/` component must be refused (ADR-006), exit code 3.
    let mut bad = std::env::temp_dir();
    bad.push("lyra_probe_guard_out.dsf");
    let _ = fs::remove_file(&bad);

    let status = Command::new(env!("CARGO_BIN_EXE_lyra-probe"))
        .arg("dsd")
        .arg("convert")
        .arg(&dff_path)
        .arg(&bad)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(3), "writing outside target/ must error (3)");
    assert!(!bad.exists(), "nothing should be written outside target/");
}
