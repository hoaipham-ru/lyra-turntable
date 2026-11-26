//! DFF -> DSF transcoding round-trip (S03, P5.3).

mod common;

use common::{build_dff, tmp_path, write_tmp};

use lyra_core::{Decoder, DsdRate, StreamSpec};
use lyra_dsd::{dff_to_dsf, open, probe, DopPacker};

fn read_all(dec: &mut dyn Decoder) -> Vec<u8> {
    let mut out = Vec::new();
    while dec.read(&mut out).unwrap() != 0 {}
    out
}

/// Deterministic per-channel test bytes (logical MSB-first values).
fn logical(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(53).wrapping_add(seed)).collect()
}

#[test]
fn dff_to_dsf_roundtrip_identical_dsd() {
    // Span several DSF blocks (block size 4096) plus a partial final block so the
    // de-blocking/padding path is exercised.
    let n = 4096 * 2 + 1234;
    let l = logical(n, 1);
    let r = logical(n, 128);

    let dff_path = write_tmp("rt.dff", &build_dff(&l, &r));
    let dsf_path = tmp_path("rt.dsf");
    dff_to_dsf(&dff_path, &dsf_path).unwrap();

    // Both report the same spec basics and length.
    let dff_info = probe(&dff_path).unwrap();
    let dsf_info = probe(&dsf_path).unwrap();
    assert_eq!(dff_info.spec.rate, DsdRate::Dsd64);
    assert_eq!(dsf_info.spec.rate, DsdRate::Dsd64);
    assert_eq!(dff_info.spec.channels, dsf_info.spec.channels);
    assert_eq!(dff_info.total_frames, dsf_info.total_frames);
    assert!(dff_info.spec.msb_first, "DFF is MSB-first");
    assert!(!dsf_info.spec.msb_first, "DSF is LSB-first");

    // Decode both; after bit-order normalisation the DSD bytes are identical.
    let mut dff = open(&dff_path).unwrap();
    let mut dsf = open(&dsf_path).unwrap();
    let dff_out = read_all(dff.as_mut());
    let dsf_out = read_all(dsf.as_mut());
    assert_eq!(dff_out.len(), dsf_out.len());
    let dsf_norm: Vec<u8> = dsf_out.iter().map(|b| b.reverse_bits()).collect();
    assert_eq!(dsf_norm, dff_out, "converted DSF must carry identical DSD");

    // And the DoP output is byte-identical: the DAC gets the same stream whichever
    // container it came from.
    let StreamSpec::Dsd(dff_spec) = dff.spec() else { panic!() };
    let StreamSpec::Dsd(dsf_spec) = dsf.spec() else { panic!() };
    let mut dop_dff = Vec::new();
    let mut dop_dsf = Vec::new();
    DopPacker::new(dff_spec).pack(&dff_out, &mut dop_dff);
    DopPacker::new(dsf_spec).pack(&dsf_out, &mut dop_dsf);
    assert_eq!(dop_dff, dop_dsf);
}

#[test]
fn dff_to_dsf_exact_block_boundary() {
    // Length an exact multiple of the block size: no partial final block.
    let n = 4096 * 3;
    let l = logical(n, 7);
    let r = logical(n, 200);
    let dff_path = write_tmp("rt2.dff", &build_dff(&l, &r));
    let dsf_path = tmp_path("rt2.dsf");
    dff_to_dsf(&dff_path, &dsf_path).unwrap();

    let mut dff = open(&dff_path).unwrap();
    let mut dsf = open(&dsf_path).unwrap();
    let dff_out = read_all(dff.as_mut());
    let dsf_out = read_all(dsf.as_mut());
    let dsf_norm: Vec<u8> = dsf_out.iter().map(|b| b.reverse_bits()).collect();
    assert_eq!(dsf_norm, dff_out);
}
