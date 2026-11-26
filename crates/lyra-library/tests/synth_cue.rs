//! Parity-style test for the synthetic gapless cue (TEST-STRATEGY §3).
//!
//! `tests/parity/synth.ps1` generates `target/corpus-synth/gapless/gapless_whole.cue`
//! (and `gapless_whole.flac`, 24/96 stereo) with 5 tracks at exact 12 s boundaries.
//! When that file is present this test asserts the resolved sample-frame boundaries
//! are exactly 0 / 1,152,000 / 2,304,000 / 3,456,000 / 4,608,000 (12 s × 96 kHz). In
//! a clean checkout the file is absent and the test skips cleanly.

use std::path::PathBuf;

use lyra_library::parse_cue;

/// Locate `target/corpus-synth/gapless/gapless_whole.cue` relative to the crate.
fn synth_cue_path() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR = <repo>/crates/lyra-library
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent()?.parent()?; // crates/ -> repo root
    let cue = repo
        .join("target")
        .join("corpus-synth")
        .join("gapless")
        .join("gapless_whole.cue");
    cue.exists().then_some(cue)
}

#[test]
fn synthetic_gapless_boundaries_are_exact() {
    let Some(cue) = synth_cue_path() else {
        eprintln!("skipping: target/corpus-synth/gapless/gapless_whole.cue not present");
        return;
    };

    let sheet = parse_cue(&cue).expect("cue parses");
    assert_eq!(sheet.files.len(), 1, "gapless_whole.cue is a single-file image");
    let file = &sheet.files[0];
    assert_eq!(
        file.sample_rate,
        Some(96_000),
        "referenced flac should probe as 96 kHz (warnings: {:?})",
        sheet.warnings
    );

    let starts: Vec<Option<u64>> = file.tracks.iter().map(|t| t.start_sampleframe).collect();
    assert_eq!(
        starts,
        vec![
            Some(0),
            Some(1_152_000),
            Some(2_304_000),
            Some(3_456_000),
            Some(4_608_000),
        ],
        "5 tracks at exact 12 s × 96 kHz boundaries"
    );

    // Each track (except the last) ends exactly where the next begins.
    for i in 0..file.tracks.len() - 1 {
        assert_eq!(
            file.tracks[i].end_sampleframe,
            file.tracks[i + 1].start_sampleframe,
            "track {i} end == track {} start (gapless)",
            i + 1
        );
    }
}
