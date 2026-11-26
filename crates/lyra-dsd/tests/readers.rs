//! DSF and DFF reader tests: synthetic files (self-contained) plus optional
//! checks against the two corpus DFF files when they are present.

use std::fs;
use std::path::{Path, PathBuf};

use lyra_core::{Decoder, DsdRate, StreamSpec};
use lyra_dsd::{open, probe, DopPacker, DsdContainer};

// --- synthetic file builders --------------------------------------------------

/// A DSDIFF chunk: id + 64-bit BE size + data, padded to an even boundary.
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

/// Build a tiny uncompressed stereo DSD64 DFF from per-channel byte vectors
/// (native MSB-first). `compression` is the `CMPR`/sound-chunk id to use.
fn build_dff(l: &[u8], r: &[u8], compression: &[u8; 4]) -> Vec<u8> {
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
    cmpr.extend_from_slice(compression);
    cmpr.push(14);
    cmpr.extend_from_slice(b"not compressed");
    snd.extend(dff_chunk(b"CMPR", &cmpr));

    let mut body = Vec::new();
    body.extend_from_slice(b"DSD "); // FRM8 form type
    body.extend(dff_chunk(b"FVER", &0x0105_0000u32.to_be_bytes()));
    body.extend(dff_chunk(b"PROP", &snd));
    body.extend(dff_chunk(compression, &sound)); // "DSD " or "DST " sound chunk

    let mut file = Vec::new();
    file.extend_from_slice(b"FRM8");
    file.extend_from_slice(&(body.len() as u64).to_be_bytes());
    file.extend(body);
    file
}

/// Build a tiny stereo DSD64 DSF from per-channel byte vectors (native
/// LSB-first) with the given block size, padding each channel's last block.
fn build_dsf(l: &[u8], r: &[u8], block_size: u32) -> Vec<u8> {
    assert_eq!(l.len(), r.len());
    let bytes_per_channel = l.len() as u64;
    let sample_count = bytes_per_channel * 8;
    let bs = block_size as u64;
    let blocks = bytes_per_channel.div_ceil(bs);

    let mut data = Vec::new();
    for g in 0..blocks {
        for ch in [l, r] {
            let start = (g * bs) as usize;
            let end = (start + bs as usize).min(ch.len());
            let slice = &ch[start..end];
            data.extend_from_slice(slice);
            // zero-pad the block up to block_size
            data.resize(data.len() + (bs as usize - slice.len()), 0);
        }
    }

    let data_chunk_size = data.len() as u64 + 12;

    // fmt chunk body (after the 12-byte id+size header).
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u32.to_le_bytes()); // format version
    fmt.extend_from_slice(&0u32.to_le_bytes()); // format id (DSD raw)
    fmt.extend_from_slice(&2u32.to_le_bytes()); // channel type (stereo)
    fmt.extend_from_slice(&2u32.to_le_bytes()); // channel num
    fmt.extend_from_slice(&2_822_400u32.to_le_bytes()); // sampling freq
    fmt.extend_from_slice(&1u32.to_le_bytes()); // bits per sample
    fmt.extend_from_slice(&sample_count.to_le_bytes()); // sample count
    fmt.extend_from_slice(&block_size.to_le_bytes()); // block size
    fmt.extend_from_slice(&0u32.to_le_bytes()); // reserved

    let mut file = Vec::new();
    // DSD chunk (28 bytes total).
    let total_file_size = 28u64 + 12 + fmt.len() as u64 + data_chunk_size;
    file.extend_from_slice(b"DSD ");
    file.extend_from_slice(&28u64.to_le_bytes());
    file.extend_from_slice(&total_file_size.to_le_bytes());
    file.extend_from_slice(&0u64.to_le_bytes()); // metadata pointer (none)
    // fmt chunk.
    file.extend_from_slice(b"fmt ");
    file.extend_from_slice(&(12u64 + fmt.len() as u64).to_le_bytes());
    file.extend_from_slice(&fmt);
    // data chunk.
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

/// Deterministic per-channel test bytes (logical MSB-first values).
fn logical(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(37).wrapping_add(seed)).collect()
}

fn read_all(dec: &mut dyn Decoder) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let before = out.len();
        let n = dec.read(&mut out).unwrap();
        if n == 0 {
            assert_eq!(out.len(), before);
            break;
        }
    }
    out
}

// --- synthetic DFF ------------------------------------------------------------

#[test]
fn dff_spec_and_length() {
    let l = logical(20, 1);
    let r = logical(20, 200);
    let path = write_tmp("tiny.dff", &build_dff(&l, &r, b"DSD "));

    let info = probe(&path).unwrap();
    assert_eq!(info.container, DsdContainer::Dff);
    assert_eq!(info.spec.rate, DsdRate::Dsd64);
    assert_eq!(info.spec.channels, 2);
    assert!(info.spec.msb_first, "DFF must be MSB-first");
    assert_eq!(info.total_frames, 20 * 8);

    let mut dec = open(&path).unwrap();
    assert!(matches!(dec.spec(), StreamSpec::Dsd(_)));
    let out = read_all(dec.as_mut());
    // Interleaved L,R bytes.
    let mut expect = Vec::new();
    for i in 0..20 {
        expect.push(l[i]);
        expect.push(r[i]);
    }
    assert_eq!(out, expect);
}

#[test]
fn dff_dst_compression_unsupported() {
    let l = logical(8, 1);
    let r = logical(8, 2);
    // CMPR says DST and the sound chunk is DST -> must be rejected.
    let path = write_tmp("dst.dff", &build_dff(&l, &r, b"DST "));
    match open(&path) {
        Err(lyra_core::LyraError::UnsupportedFormat(_)) => {}
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!("expected Unsupported, DST DFF opened"),
    }
}

// --- synthetic DSF ------------------------------------------------------------

#[test]
fn dsf_spec_and_deblock() {
    // Logical (MSB-first) bytes; DSF stores them LSB-first (bit-reversed).
    let l = logical(20, 1);
    let r = logical(20, 200);
    let l_stored: Vec<u8> = l.iter().map(|b| b.reverse_bits()).collect();
    let r_stored: Vec<u8> = r.iter().map(|b| b.reverse_bits()).collect();
    // block_size 8 -> 3 blocks per channel (8,8,4), exercises de-blocking.
    let path = write_tmp("tiny.dsf", &build_dsf(&l_stored, &r_stored, 8));

    let info = probe(&path).unwrap();
    assert_eq!(info.container, DsdContainer::Dsf);
    assert_eq!(info.spec.rate, DsdRate::Dsd64);
    assert_eq!(info.spec.channels, 2);
    assert!(!info.spec.msb_first, "DSF must be LSB-first");
    assert_eq!(info.total_frames, 20 * 8);

    let mut dec = open(&path).unwrap();
    let out = read_all(dec.as_mut());
    // De-blocked, interleaved stored (LSB-first) bytes.
    let mut expect = Vec::new();
    for i in 0..20 {
        expect.push(l_stored[i]);
        expect.push(r_stored[i]);
    }
    assert_eq!(out, expect);
}

#[test]
fn dsf_seek_byte_boundary() {
    let l = logical(32, 5);
    let r = logical(32, 9);
    let l_stored: Vec<u8> = l.iter().map(|b| b.reverse_bits()).collect();
    let r_stored: Vec<u8> = r.iter().map(|b| b.reverse_bits()).collect();
    let path = write_tmp("seek.dsf", &build_dsf(&l_stored, &r_stored, 8));

    let mut dec = open(&path).unwrap();
    // Seek into the middle of a block (frame 100 -> byte 12 per channel, floor).
    let landed = dec.seek(100).unwrap();
    assert_eq!(landed, 96); // 12 bytes * 8
    let out = read_all(dec.as_mut());
    let mut expect = Vec::new();
    for i in 12..32 {
        expect.push(l_stored[i]);
        expect.push(r_stored[i]);
    }
    assert_eq!(out, expect);

    assert!(dec.seek(10_000).is_err());
}

// --- cross-reader equality ----------------------------------------------------

#[test]
fn cross_reader_same_logical_dsd() {
    let l = logical(40, 3);
    let r = logical(40, 77);
    let l_stored: Vec<u8> = l.iter().map(|b| b.reverse_bits()).collect();
    let r_stored: Vec<u8> = r.iter().map(|b| b.reverse_bits()).collect();

    let dff_path = write_tmp("cross.dff", &build_dff(&l, &r, b"DSD "));
    let dsf_path = write_tmp("cross.dsf", &build_dsf(&l_stored, &r_stored, 8));

    let mut dff = open(&dff_path).unwrap();
    let mut dsf = open(&dsf_path).unwrap();
    let dff_out = read_all(dff.as_mut());
    let dsf_out = read_all(dsf.as_mut());

    // Same logical DSD after normalising bit order.
    assert_eq!(dff_out.len(), dsf_out.len());
    let dsf_normalised: Vec<u8> = dsf_out.iter().map(|b| b.reverse_bits()).collect();
    assert_eq!(dsf_normalised, dff_out);

    // And the DoP output must be byte-identical: the DAC gets the same stream
    // regardless of container (DFF msb_first=true, DSF msb_first=false).
    let StreamSpec::Dsd(dff_spec) = dff.spec() else { panic!() };
    let StreamSpec::Dsd(dsf_spec) = dsf.spec() else { panic!() };
    let mut dop_dff = Vec::new();
    let mut dop_dsf = Vec::new();
    DopPacker::new(dff_spec).pack(&dff_out, &mut dop_dff);
    DopPacker::new(dsf_spec).pack(&dsf_out, &mut dop_dsf);
    assert_eq!(dop_dff, dop_dsf, "DoP must be container-independent");
}

// --- corpus (optional) --------------------------------------------------------

const CORPUS: &[&str] = &[
    r"C:\SoulSeek_Music\complete\Gavin Newsom\Ultimate Sound Test @ 192kHz 24bit FLAC + DSD 2.8MHz 1bit DSF\Beethoven Sonate 32 - Maestoso 2L49SACD_tr1_DSD_stereo.dff",
    r"C:\SoulSeek_Music\complete\qiman88\Czech Phil Plays STUDIO GHIBLI ____ [2005]\1-01 - 1st movement The Legend of Ashitaka.dff",
];

#[test]
fn corpus_dff_specs_and_length() {
    for p in CORPUS {
        let path = Path::new(p);
        if !path.exists() {
            eprintln!("skip (absent): {p}");
            continue;
        }
        let info = probe(path).unwrap();
        assert_eq!(info.container, DsdContainer::Dff);
        assert_eq!(info.spec.rate, DsdRate::Dsd64, "{p}");
        assert_eq!(info.spec.channels, 2, "{p}");
        assert!(info.spec.msb_first, "{p}");
        assert!(info.total_frames > 0 && info.total_frames.is_multiple_of(8), "{p}");

        // Decode a small portion and check the byte count matches frames.
        let mut dec = open(path).unwrap();
        let mut out = Vec::new();
        let frames = dec.read(&mut out).unwrap();
        assert!(frames > 0);
        assert_eq!(out.len(), frames / 8 * 2, "bytes = frames/8 * channels");
        eprintln!("{p}: total_frames={}", info.total_frames);
    }
}
