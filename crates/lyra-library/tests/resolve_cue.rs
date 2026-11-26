//! Self-contained sample-frame resolution test: generate a tiny real WAV file,
//! reference it from a cue, and check that CD-frame positions resolve to exact
//! source sample frames (no corpus needed).

use std::path::Path;

use lyra_library::parse_cue;

/// Build a minimal PCM WAV (`rate` Hz, 2 ch, 16-bit) holding `frames` silent
/// frames, as a byte vector.
fn tiny_wav(rate: u32, frames: u32) -> Vec<u8> {
    let channels: u16 = 2;
    let bits: u16 = 16;
    let block_align = channels * bits / 8;
    let byte_rate = rate * u32::from(block_align);
    let data_len = frames * u32::from(block_align);

    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
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
    v.extend_from_slice(&data_len.to_le_bytes());
    v.resize(v.len() + data_len as usize, 0);
    v
}

/// Create a unique temp directory for this test.
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!(
        "lyra_cue_{tag}_{}_{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

#[test]
fn single_file_wav_resolves_boundaries() {
    let dir = temp_dir("single");
    // 44.1 kHz: 1 CD frame = 44100 / 75 = 588 sample frames.
    let total_frames = 1_000u32;
    std::fs::write(dir.join("whole.wav"), tiny_wav(44_100, total_frames)).unwrap();

    let cue = "\
TITLE \"Album\"
FILE \"whole.wav\" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:00:01
";
    let cue_path = dir.join("whole.cue");
    std::fs::write(&cue_path, cue).unwrap();

    let sheet = parse_cue(&cue_path).unwrap();
    assert!(sheet.warnings.is_empty(), "unexpected warnings: {:?}", sheet.warnings);
    let f = &sheet.files[0];
    assert_eq!(f.sample_rate, Some(44_100));
    assert_eq!(f.total_sampleframes, Some(u64::from(total_frames)));

    assert_eq!(f.tracks[0].start_sampleframe, Some(0));
    assert_eq!(f.tracks[0].end_sampleframe, Some(588)); // next track INDEX 01
    assert_eq!(f.tracks[1].start_sampleframe, Some(588));
    assert_eq!(f.tracks[1].end_sampleframe, Some(1_000)); // EOF
    // In-file pregap of track 2 is INDEX00..INDEX01 = 1 CD frame = 588 frames.
    assert_eq!(f.tracks[1].pregap_sampleframes, Some(588));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn multi_file_wav_each_ends_at_eof() {
    let dir = temp_dir("multi");
    std::fs::write(dir.join("a.wav"), tiny_wav(48_000, 480)).unwrap();
    std::fs::write(dir.join("b.wav"), tiny_wav(48_000, 960)).unwrap();

    let cue = "\
FILE \"a.wav\" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
FILE \"b.wav\" WAVE
  TRACK 02 AUDIO
    INDEX 01 00:00:00
";
    let cue_path = dir.join("album.cue");
    std::fs::write(&cue_path, cue).unwrap();

    let sheet = parse_cue(&cue_path).unwrap();
    assert_eq!(sheet.files.len(), 2);
    assert_eq!(Path::new(&sheet.files[0].path).file_name().unwrap(), "a.wav");
    // Each single-track file ends at its own EOF.
    assert_eq!(sheet.files[0].tracks[0].start_sampleframe, Some(0));
    assert_eq!(sheet.files[0].tracks[0].end_sampleframe, Some(480));
    assert_eq!(sheet.files[1].tracks[0].end_sampleframe, Some(960));

    std::fs::remove_dir_all(&dir).ok();
}
