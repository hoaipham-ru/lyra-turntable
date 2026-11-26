//! `.cue` sheet parsing (S06, P3.2).
//!
//! A cue sheet describes how one or more audio *image* files are split into
//! tracks. This module parses both common layouts:
//!
//! * **Single-file image:** one `FILE` followed by several `TRACK`s whose
//!   `INDEX` positions point into that one file (a gapless album rip).
//! * **Multi-file:** each `TRACK` sits under its own `FILE` (one file per track).
//!
//! # Time and sample frames
//! Cue positions are written as `MM:SS:FF` where `FF` is a **CD frame** and there
//! are exactly **75 CD frames per second** (so one CD frame is 1/75 s). That is
//! coarser than audio samples, so to give the engine frame-accurate boundaries we
//! must know each referenced file's sample rate. [`parse_cue`] therefore probes
//! every referenced file (via [`lyra_decode::probe`] for PCM, [`lyra_dsd::probe`]
//! for DSD) and converts every CD-frame position to a **source sample frame** with
//! exact integer math:
//!
//! ```text
//! sample_frame = cd_frame * sample_rate / 75
//! ```
//!
//! Both the raw CD-frame value and the resolved sample-frame value are kept on
//! each [`CueTrack`], so a sheet whose files could not be probed is still returned
//! (with the sample-frame fields left `None` and a message in
//! [`CueSheet::warnings`]) rather than failing the whole parse.
//!
//! # Robustness
//! The parser tolerates CRLF or LF line endings, a leading UTF-8 BOM, quoted and
//! unquoted strings, mixed-case keywords, extra whitespace and unknown commands
//! (which are skipped). It never panics on malformed input; problems that change
//! the result are collected in [`CueSheet::warnings`].

use std::fs;
use std::path::{Path, PathBuf};

use lyra_core::{LyraError, Result, StreamSpec};

/// Number of CD frames in one second (the cue-sheet time base).
pub const CD_FRAMES_PER_SECOND: u64 = 75;

/// A parsed cue sheet: its disc-level metadata and one or more referenced files,
/// each carrying its own tracks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CueSheet {
    /// The referenced audio files, in the order they appear in the sheet.
    pub files: Vec<CueFile>,
    /// Disc-level `TITLE` (the album title), if the header set one.
    pub disc_title: Option<String>,
    /// Disc-level `PERFORMER` (the album artist), if the header set one.
    pub disc_performer: Option<String>,
    /// Disc-level `SONGWRITER`, if the header set one.
    pub disc_songwriter: Option<String>,
    /// Disc-level `REM key value` pairs (e.g. `GENRE`, `DATE`, `DISCID`,
    /// `REPLAYGAIN_ALBUM_GAIN`). Keys are upper-cased; values keep their original
    /// case with surrounding quotes removed.
    pub rem: Vec<(String, String)>,
    /// Non-fatal problems found while parsing or resolving (unknown time fields,
    /// a file that could not be probed, a track without an `INDEX 01`, …). An
    /// empty vector means a clean parse.
    pub warnings: Vec<String>,
}

impl CueSheet {
    /// Iterate every track in the sheet together with the file it belongs to, in
    /// sheet order. Convenience for consumers that want a flat virtual-track list.
    pub fn tracks(&self) -> impl Iterator<Item = (&CueFile, &CueTrack)> {
        self.files
            .iter()
            .flat_map(|f| f.tracks.iter().map(move |t| (f, t)))
    }
}

/// One `FILE` entry and the tracks that index into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueFile {
    /// The referenced file path, resolved relative to the cue sheet's directory
    /// (absolute paths in the sheet are kept as written). The file is never
    /// required to exist for parsing to succeed.
    pub path: PathBuf,
    /// The file name exactly as written inside the sheet (before resolution).
    pub raw_name: String,
    /// The `FILE` type token as written, upper-cased (`WAVE`, `MP3`, `BINARY`,
    /// `AIFF`, …). Empty if the sheet omitted it.
    pub format: String,
    /// The file's audio sample rate in Hz once probed, or `None` if the file could
    /// not be read. For DSD this is the one-bit sample rate (e.g. `2_822_400` for
    /// DSD64), so a "sample frame" is one DSD sample per channel.
    pub sample_rate: Option<u32>,
    /// Exact length of the file in source sample frames once probed, used as the
    /// end boundary of the last track in the file. `None` if unprobed/unknown.
    pub total_sampleframes: Option<u64>,
    /// The tracks that index into this file, in sheet order.
    pub tracks: Vec<CueTrack>,
}

/// One `TRACK` inside a [`CueFile`].
///
/// Every position is kept twice: the raw CD-frame value from the sheet (always
/// present) and the resolved source sample-frame value (`Some` only once the
/// owning file's sample rate is known).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueTrack {
    /// The track number from `TRACK nn` (usually 1-based and sequential, but taken
    /// verbatim from the sheet).
    pub number: u32,
    /// Track `TITLE`, if present.
    pub title: Option<String>,
    /// Track `PERFORMER`, if present.
    pub performer: Option<String>,
    /// Track `SONGWRITER`, if present.
    pub songwriter: Option<String>,
    /// Track-level `REM key value` pairs (see [`CueSheet::rem`]).
    pub rem: Vec<(String, String)>,

    /// `INDEX 00` position in CD frames — the start of the pregap — if present.
    pub index00_cdframes: Option<u64>,
    /// `INDEX 01` position in CD frames — the audible start of the track.
    pub index01_cdframes: u64,
    /// Explicit `PREGAP MM:SS:FF` duration in CD frames, if the command appeared.
    /// This is a *silent* pregap not present in the audio file, distinct from the
    /// `INDEX 00`→`INDEX 01` gap (which is audio in the file).
    pub pregap_cdframes: Option<u64>,
    /// Explicit `POSTGAP MM:SS:FF` duration in CD frames, if present.
    pub postgap_cdframes: Option<u64>,

    /// Track start (`INDEX 01`) in source sample frames, once the rate is known.
    pub start_sampleframe: Option<u64>,
    /// Track end in source sample frames (exclusive): the next track's start, or
    /// end-of-file for the last track in the file. `Some` once the rate is known.
    pub end_sampleframe: Option<u64>,
    /// Effective in-file pregap (`INDEX 00`→`INDEX 01` gap) in source sample
    /// frames, once the rate is known. `None` if the track has no `INDEX 00`.
    pub pregap_sampleframes: Option<u64>,
}

impl CueTrack {
    /// The effective in-file pregap in CD frames: the gap between `INDEX 00` and
    /// `INDEX 01`, or `None` when the track has no `INDEX 00`.
    ///
    /// This is the portion of the file that plays before the track proper. It is
    /// independent of the explicit [`pregap_cdframes`](Self::pregap_cdframes)
    /// command (which inserts silence that is not in the file).
    #[must_use]
    pub fn in_file_pregap_cdframes(&self) -> Option<u64> {
        self.index00_cdframes
            .map(|i0| self.index01_cdframes.saturating_sub(i0))
    }
}

/// Parse the cue sheet at `path`.
///
/// Reads the sheet (never the referenced audio) with read-only access, parses its
/// structure tolerantly, then probes each referenced file to resolve every
/// CD-frame position into source sample frames. A file that is missing or
/// unreadable does not fail the parse: its [`CueFile::sample_rate`] stays `None`,
/// its tracks keep CD-frame positions with `None` sample-frame fields, and a note
/// is added to [`CueSheet::warnings`].
///
/// # Errors
/// Returns [`LyraError::Io`] only if the cue sheet file itself cannot be read.
pub fn parse_cue(path: &Path) -> Result<CueSheet> {
    let bytes = fs::read(path)
        .map_err(|e| LyraError::Io(format!("reading cue sheet {}: {e}", path.display())))?;
    let text = decode_text(&bytes);
    let mut sheet = parse_text(&text, path.parent());
    resolve_sample_frames(&mut sheet);
    Ok(sheet)
}

/// Strip a leading UTF-8 BOM and decode bytes as UTF-8 lossily.
fn decode_text(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(body).into_owned()
}

/// Convert a CD-frame count to a source sample-frame count at `rate` Hz, exactly.
///
/// Uses 128-bit intermediate math so `cd_frames * rate` never overflows, then an
/// exact integer division by [`CD_FRAMES_PER_SECOND`]. For every standard audio
/// rate (44.1/48/88.2/96/176.4/192 kHz and the DSD rates) the division is exact.
#[must_use]
fn cd_to_sampleframe(cd_frames: u64, rate: u32) -> u64 {
    let n = u128::from(cd_frames) * u128::from(rate) / u128::from(CD_FRAMES_PER_SECOND);
    n as u64
}

/// Parse `MM:SS:FF` into a CD-frame count. Returns `(frames, warning)` where the
/// warning is `Some` if a field was malformed or out of range (the value is still
/// parsed best-effort).
fn parse_time(s: &str) -> (u64, Option<String>) {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return (0, Some(format!("malformed time {s:?}: expected MM:SS:FF")));
    }
    let parse = |p: &str| p.trim().parse::<u64>().ok();
    let (mm, ss, ff) = (parse(parts[0]), parse(parts[1]), parse(parts[2]));
    match (mm, ss, ff) {
        (Some(mm), Some(ss), Some(ff)) => {
            let mut warn = None;
            if ss >= 60 || ff >= CD_FRAMES_PER_SECOND {
                warn = Some(format!(
                    "time {s:?} out of range (SS<60, FF<75); parsed best-effort"
                ));
            }
            ((mm * 60 + ss) * CD_FRAMES_PER_SECOND + ff, warn)
        }
        _ => (0, Some(format!("malformed time {s:?}: non-numeric field"))),
    }
}

/// Split a line into its first whitespace-delimited word and the trimmed
/// remainder.
fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

/// Remove one layer of surrounding double quotes, if present.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Interpret a string argument: the contents of a leading quoted run, otherwise
/// the whole (trimmed) remainder — so unquoted values with embedded spaces survive.
fn string_arg(rest: &str) -> String {
    let rest = rest.trim();
    if let Some(stripped) = rest.strip_prefix('"') {
        if let Some(end) = stripped.find('"') {
            return stripped[..end].to_string();
        }
    }
    rest.to_string()
}

/// Parse a `FILE` argument of the form `"name" TYPE` or `name TYPE` into the file
/// name and the (possibly empty) upper-cased type token.
fn parse_file_arg(rest: &str) -> (String, String) {
    let rest = rest.trim();
    if let Some(stripped) = rest.strip_prefix('"') {
        if let Some(end) = stripped.find('"') {
            let name = stripped[..end].to_string();
            let typ = stripped[end + 1..].trim().to_ascii_uppercase();
            return (name, typ);
        }
        // Unterminated quote: take the rest as the name.
        return (stripped.to_string(), String::new());
    }
    // Unquoted: the type is the last token, the name is everything before it.
    match rest.rsplit_once(char::is_whitespace) {
        Some((name, typ)) => (name.trim().to_string(), typ.trim().to_ascii_uppercase()),
        None => (rest.to_string(), String::new()),
    }
}

/// Parse the sheet text into structure (CD-frame positions only; no probing).
fn parse_text(text: &str, base_dir: Option<&Path>) -> CueSheet {
    let mut sheet = CueSheet::default();
    let mut cur_track: Option<CueTrack> = None;
    let mut cur_file: Option<usize> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let (kw, rest) = split_first_word(line);
        match kw.to_ascii_uppercase().as_str() {
            "FILE" => {
                finish_track(&mut sheet, &mut cur_file, &mut cur_track);
                let (name, format) = parse_file_arg(rest);
                let path = resolve_path(base_dir, &name);
                sheet.files.push(CueFile {
                    path,
                    raw_name: name,
                    format,
                    sample_rate: None,
                    total_sampleframes: None,
                    tracks: Vec::new(),
                });
                cur_file = Some(sheet.files.len() - 1);
            }
            "TRACK" => {
                finish_track(&mut sheet, &mut cur_file, &mut cur_track);
                let (num, _ty) = split_first_word(rest);
                let number = num.trim().parse::<u32>().unwrap_or_else(|_| {
                    sheet
                        .warnings
                        .push(format!("TRACK number {num:?} not numeric; using 0"));
                    0
                });
                if cur_file.is_none() {
                    sheet
                        .warnings
                        .push(format!("TRACK {number} appears before any FILE; skipped"));
                    cur_track = None;
                    continue;
                }
                cur_track = Some(CueTrack {
                    number,
                    title: None,
                    performer: None,
                    songwriter: None,
                    rem: Vec::new(),
                    index00_cdframes: None,
                    index01_cdframes: 0,
                    pregap_cdframes: None,
                    postgap_cdframes: None,
                    start_sampleframe: None,
                    end_sampleframe: None,
                    pregap_sampleframes: None,
                });
            }
            "INDEX" => {
                let (num, time) = split_first_word(rest);
                let (frames, warn) = parse_time(time);
                if let Some(w) = warn {
                    sheet.warnings.push(w);
                }
                if let Some(t) = cur_track.as_mut() {
                    match num.trim() {
                        "0" | "00" => t.index00_cdframes = Some(frames),
                        "1" | "01" => t.index01_cdframes = frames,
                        _ => {} // higher INDEX points are not track boundaries; ignore.
                    }
                } else {
                    sheet
                        .warnings
                        .push("INDEX outside any TRACK; ignored".to_string());
                }
            }
            "PREGAP" => {
                let (frames, warn) = parse_time(rest);
                if let Some(w) = warn {
                    sheet.warnings.push(w);
                }
                if let Some(t) = cur_track.as_mut() {
                    t.pregap_cdframes = Some(frames);
                }
            }
            "POSTGAP" => {
                let (frames, warn) = parse_time(rest);
                if let Some(w) = warn {
                    sheet.warnings.push(w);
                }
                if let Some(t) = cur_track.as_mut() {
                    t.postgap_cdframes = Some(frames);
                }
            }
            "TITLE" => set_field(&mut sheet, &mut cur_track, FieldKind::Title, rest),
            "PERFORMER" => set_field(&mut sheet, &mut cur_track, FieldKind::Performer, rest),
            "SONGWRITER" => set_field(&mut sheet, &mut cur_track, FieldKind::Songwriter, rest),
            "REM" => {
                let (key, val) = split_first_word(rest);
                let pair = (key.to_ascii_uppercase(), unquote(val).to_string());
                match cur_track.as_mut() {
                    Some(t) => t.rem.push(pair),
                    None => sheet.rem.push(pair),
                }
            }
            // Known-but-unused commands and anything unknown: skip gracefully.
            _ => {}
        }
    }
    finish_track(&mut sheet, &mut cur_file, &mut cur_track);
    sheet
}

/// Which text field a `TITLE`/`PERFORMER`/`SONGWRITER` line sets.
enum FieldKind {
    Title,
    Performer,
    Songwriter,
}

/// Apply a text field to the current track, or to the disc header if no track is
/// open yet.
fn set_field(sheet: &mut CueSheet, cur_track: &mut Option<CueTrack>, kind: FieldKind, rest: &str) {
    let value = string_arg(rest);
    match cur_track.as_mut() {
        Some(t) => match kind {
            FieldKind::Title => t.title = Some(value),
            FieldKind::Performer => t.performer = Some(value),
            FieldKind::Songwriter => t.songwriter = Some(value),
        },
        None => match kind {
            FieldKind::Title => sheet.disc_title = Some(value),
            FieldKind::Performer => sheet.disc_performer = Some(value),
            FieldKind::Songwriter => sheet.disc_songwriter = Some(value),
        },
    }
}

/// Push the current track (if any) into its file, warning if it had no `INDEX 01`.
fn finish_track(sheet: &mut CueSheet, cur_file: &mut Option<usize>, cur_track: &mut Option<CueTrack>) {
    let Some(mut track) = cur_track.take() else {
        return;
    };
    let Some(fi) = *cur_file else {
        return;
    };
    // A track with only INDEX 00 (no INDEX 01) is unusual. `index01_cdframes`
    // defaults to 0 when the command was absent, so a non-zero INDEX 00 with a
    // zero INDEX 01 means INDEX 01 was never given: promote INDEX 00 to the start
    // so the track still has a position, and note it.
    if track.index01_cdframes == 0 {
        if let Some(i0) = track.index00_cdframes.filter(|&i0| i0 != 0) {
            sheet.warnings.push(format!(
                "TRACK {} has INDEX 00 but no INDEX 01; using INDEX 00 as start",
                track.number
            ));
            track.index01_cdframes = i0;
            track.index00_cdframes = None;
        }
    }
    sheet.files[fi].tracks.push(track);
}

/// Resolve a `FILE` name against the sheet directory. Absolute names are kept.
fn resolve_path(base_dir: Option<&Path>, name: &str) -> PathBuf {
    let p = Path::new(name);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match base_dir {
        Some(dir) => dir.join(p),
        None => p.to_path_buf(),
    }
}

/// Probe each file's sample rate and length, then fill in every sample-frame field.
fn resolve_sample_frames(sheet: &mut CueSheet) {
    let mut warnings = Vec::new();
    for file in &mut sheet.files {
        let (rate, total) = match probe_file(&file.path) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!(
                    "cue references {:?}: {e}; sample-frame positions left unresolved",
                    file.raw_name
                ));
                continue;
            }
        };
        file.sample_rate = Some(rate);
        file.total_sampleframes = total;

        let n = file.tracks.len();
        for t in &mut file.tracks {
            t.start_sampleframe = Some(cd_to_sampleframe(t.index01_cdframes, rate));
            t.pregap_sampleframes = t
                .in_file_pregap_cdframes()
                .map(|cd| cd_to_sampleframe(cd, rate));
        }
        for i in 0..n {
            let end = if i + 1 < n {
                file.tracks[i + 1].start_sampleframe
            } else {
                total
            };
            file.tracks[i].end_sampleframe = end;
        }
    }
    sheet.warnings.extend(warnings);
}

/// Probe a referenced file for `(sample_rate_hz, total_sample_frames)`.
///
/// Dispatches on extension: `.dsf`/`.dff` go to [`lyra_dsd::probe`] (rate is the
/// one-bit DSD rate), everything else to [`lyra_decode::probe`].
fn probe_file(path: &Path) -> Result<(u32, Option<u64>)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("dsf") | Some("dff") => {
            let info = lyra_dsd::probe(path)?;
            Ok((info.spec.rate.hz(), Some(info.total_frames)))
        }
        _ => {
            let info = lyra_decode::probe(path)?;
            let rate = match info.spec {
                StreamSpec::Pcm(p) => p.rate,
                StreamSpec::Dsd(d) => d.rate.hz(),
            };
            Ok((rate, info.total_frames))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_frame_math_is_exact() {
        // 12 s = 900 CD frames; at 96 kHz that is 1,152,000 sample frames.
        assert_eq!(cd_to_sampleframe(900, 96_000), 1_152_000);
        // 1 CD frame at 44.1 kHz = 588 sample frames (44100 / 75).
        assert_eq!(cd_to_sampleframe(1, 44_100), 588);
        // DSD64 one-bit rate: 1 CD frame = 2822400 / 75 = 37632.
        assert_eq!(cd_to_sampleframe(1, 2_822_400), 37_632);
        // No overflow at a large position (2 hours) * 192 kHz.
        let two_hours = 2 * 60 * 60 * CD_FRAMES_PER_SECOND;
        assert_eq!(cd_to_sampleframe(two_hours, 192_000), 2 * 60 * 60 * 192_000);
    }

    #[test]
    fn time_parsing() {
        assert_eq!(parse_time("00:00:00").0, 0);
        assert_eq!(parse_time("00:12:00").0, 12 * 75);
        assert_eq!(parse_time("01:02:03").0, (60 + 2) * 75 + 3);
        // Out of range frame field is flagged but still parsed.
        let (frames, warn) = parse_time("00:00:99");
        assert_eq!(frames, 99);
        assert!(warn.is_some());
        // Malformed shape is flagged.
        assert!(parse_time("1:2").1.is_some());
    }

    #[test]
    fn file_arg_parsing() {
        assert_eq!(
            parse_file_arg("\"my track.flac\" WAVE"),
            ("my track.flac".to_string(), "WAVE".to_string())
        );
        assert_eq!(
            parse_file_arg("track.flac wave"),
            ("track.flac".to_string(), "WAVE".to_string())
        );
        assert_eq!(
            parse_file_arg("\"only.flac\""),
            ("only.flac".to_string(), String::new())
        );
    }

    #[test]
    fn single_file_image_cdframes() {
        let cue = "\
REM GENRE Ambient
REM DATE 2020
PERFORMER \"The Band\"
TITLE \"The Album\"
FILE \"whole.flac\" WAVE
  TRACK 01 AUDIO
    TITLE \"One\"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE \"Two\"
    INDEX 00 00:11:50
    INDEX 01 00:12:00
  TRACK 03 AUDIO
    TITLE \"Three\"
    INDEX 01 00:24:00
";
        let sheet = parse_text(cue, None);
        assert_eq!(sheet.disc_title.as_deref(), Some("The Album"));
        assert_eq!(sheet.disc_performer.as_deref(), Some("The Band"));
        assert_eq!(sheet.rem, vec![
            ("GENRE".to_string(), "Ambient".to_string()),
            ("DATE".to_string(), "2020".to_string()),
        ]);
        assert_eq!(sheet.files.len(), 1);
        let f = &sheet.files[0];
        assert_eq!(f.raw_name, "whole.flac");
        assert_eq!(f.format, "WAVE");
        assert_eq!(f.tracks.len(), 3);
        assert_eq!(f.tracks[0].index01_cdframes, 0);
        assert_eq!(f.tracks[0].title.as_deref(), Some("One"));
        assert_eq!(f.tracks[1].index00_cdframes, Some(11 * 75 + 50));
        assert_eq!(f.tracks[1].index01_cdframes, 12 * 75);
        assert_eq!(f.tracks[1].in_file_pregap_cdframes(), Some(25));
        assert_eq!(f.tracks[2].index01_cdframes, 24 * 75);
    }

    #[test]
    fn multi_file_cue() {
        let cue = "\
FILE \"a.flac\" WAVE
  TRACK 01 AUDIO
    TITLE \"A\"
    INDEX 01 00:00:00
FILE \"b.flac\" WAVE
  TRACK 02 AUDIO
    TITLE \"B\"
    INDEX 01 00:00:00
";
        let sheet = parse_text(cue, None);
        assert_eq!(sheet.files.len(), 2);
        assert_eq!(sheet.files[0].tracks.len(), 1);
        assert_eq!(sheet.files[1].tracks.len(), 1);
        assert_eq!(sheet.files[0].tracks[0].title.as_deref(), Some("A"));
        assert_eq!(sheet.files[1].tracks[0].number, 2);
    }

    #[test]
    fn crlf_bom_case_quoting_and_unknown_commands() {
        // Leading BOM, CRLF endings, lower/mixed case keywords, unquoted title with
        // spaces, an unknown command, and an explicit PREGAP.
        let raw = "\u{feff}rem GeNrE Jazz\r\nTiTlE An Unquoted Title\r\nFLAGS DCP\r\nSOMEWEIRDCMD foo bar\r\nfile \"x.flac\" wave\r\n  track 1 audio\r\n    pregap 00:00:32\r\n    index 01 00:00:00\r\n";
        let bytes = {
            let mut v = Vec::new();
            v.extend_from_slice(raw.as_bytes());
            v
        };
        let text = decode_text(&bytes);
        let sheet = parse_text(&text, None);
        assert_eq!(sheet.disc_title.as_deref(), Some("An Unquoted Title"));
        assert_eq!(sheet.rem, vec![("GENRE".to_string(), "Jazz".to_string())]);
        assert_eq!(sheet.files.len(), 1);
        assert_eq!(sheet.files[0].format, "WAVE");
        let t = &sheet.files[0].tracks[0];
        assert_eq!(t.number, 1);
        assert_eq!(t.pregap_cdframes, Some(32));
        assert_eq!(t.index01_cdframes, 0);
    }

    #[test]
    fn missing_file_leaves_sample_frames_unresolved_and_reports() {
        let dir = std::env::temp_dir().join(format!("lyra_cue_missing_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cue_path = dir.join("missing.cue");
        std::fs::write(
            &cue_path,
            "FILE \"does_not_exist.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        let sheet = parse_cue(&cue_path).unwrap();
        assert_eq!(sheet.files.len(), 1);
        assert!(sheet.files[0].sample_rate.is_none());
        assert_eq!(sheet.files[0].tracks[0].index01_cdframes, 0);
        assert!(sheet.files[0].tracks[0].start_sampleframe.is_none());
        assert!(sheet.files[0].tracks[0].end_sampleframe.is_none());
        assert!(
            sheet.warnings.iter().any(|w| w.contains("does_not_exist.flac")),
            "expected a warning naming the missing file, got {:?}",
            sheet.warnings
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn track_without_index01_uses_index00() {
        let cue = "\
FILE \"a.flac\" WAVE
  TRACK 01 AUDIO
    INDEX 00 00:05:00
";
        let sheet = parse_text(cue, None);
        assert_eq!(sheet.files[0].tracks[0].index01_cdframes, 5 * 75);
        assert!(sheet.warnings.iter().any(|w| w.contains("no INDEX 01")));
    }
}
