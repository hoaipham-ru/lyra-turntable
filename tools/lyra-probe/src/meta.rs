//! `lyra-probe meta` — dump a file's foobar2000-parity metadata (S09, P6.5).
//!
//! Prints the TEST-STRATEGY §6 field set for one file so the P6 harness can diff
//! it, field by field, against foobar2000's Beefweb columns (`fb2k-meta.json`).
//! The §6 columns are emitted under **exactly** these JSON keys:
//!
//! `title, artist, album, album_artist, tracknumber, discnumber, date, genre,
//! length_samples, samplerate, bitspersample, channels, codec,
//! replaygain_track_gain, replaygain_album_gain`.
//!
//! # Where each field comes from
//! * **PCM / lossy** (FLAC, WAV, MP3, …): tag and format fields come from
//!   [`lyra_library::read_tags`] (read-only, never decodes). The **exact**
//!   `length_samples` comes from the decoder probe ([`lyra_decode::probe`]) — the
//!   same decode path `decode`/`compare` use — because the tag-derived
//!   `duration_frames` is only approximate (duration × rate, rounded). When the
//!   decoder does not yet support the container (e.g. MP3 is P1b, not P6),
//!   `length_samples` is reported `null` with the approximate count kept in the
//!   diagnostics, rather than reporting the approximate value as if it were exact.
//! * **DSD** (`.dsf`/`.dff`): `read_tags` returns `UnsupportedFormat` (lofty 0.25.5
//!   has no DSF/DFF `FileType`), so the codec/rate/channels/length all come from
//!   the `lyra-dsd` reader ([`lyra_dsd::probe`]): `codec = "DSD"`,
//!   `samplerate` = the DSD bit rate in Hz, `channels`, `bitspersample = 1`,
//!   `length_samples` = the exact DSD sample count per channel (`total_frames`).
//!
//! # Diagnostics (non-§6 keys)
//! The JSON also carries a few keys the §6 comparison ignores but the internal
//! consistency check uses: `_source` (`"pcm"`/`"dsd"`), `_duration_frames_approx`
//! (the tag-derived count), and `_decoder` (the decoder's own
//! samplerate/channels/bits/total_frames) so the harness can assert the tag-
//! reported format equals the decoder's even without foobar2000.
//!
//! Exit-code contract: 0 ok, 3 harness/IO/unsupported error.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use lyra_core::StreamSpec;
use serde::Serialize;

/// The decoder's own view of a file's format (diagnostics for the consistency
/// check). `None` when no Lyra decoder supports the container yet.
#[derive(Debug, Clone, Serialize)]
struct DecoderView {
    samplerate: Option<u32>,
    channels: Option<u16>,
    bits: Option<u8>,
    total_frames: Option<u64>,
}

/// What Lyra's read-only artwork lookup found for a file (diagnostics for the
/// artwork section of the P6 report). Never fatal: a broken embedded picture or an
/// unreadable folder image yields `found = false` rather than erroring.
#[derive(Debug, Clone, Serialize)]
struct ArtworkView {
    found: bool,
    /// `"embedded"` (an APIC/METADATA_BLOCK_PICTURE), `"folder"` (a sidecar image)
    /// or `null` when none was found.
    source: Option<&'static str>,
    mime: Option<String>,
    /// The APIC picture type for an embedded image (e.g. `"CoverFront"`); `None`
    /// for a folder image or when nothing was found.
    picture_type: Option<String>,
    bytes: Option<usize>,
    /// Set only if the folder lookup hit a read error (reported, not fatal).
    error: Option<String>,
}

/// The full §6 field set plus diagnostics for one file.
#[derive(Debug, Clone, Serialize)]
struct Meta {
    path: String,
    // --- TEST-STRATEGY §6 columns (keys must match fb2k-meta.json) ---
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    tracknumber: Option<String>,
    discnumber: Option<String>,
    date: Option<String>,
    genre: Option<String>,
    length_samples: Option<u64>,
    samplerate: Option<u32>,
    bitspersample: Option<u8>,
    channels: Option<u16>,
    codec: Option<String>,
    replaygain_track_gain: Option<String>,
    replaygain_album_gain: Option<String>,
    // --- diagnostics (ignored by the §6 diff) ---
    #[serde(rename = "_source")]
    source: &'static str,
    #[serde(rename = "_duration_frames_approx")]
    duration_frames_approx: Option<u64>,
    #[serde(rename = "_decoder")]
    decoder: DecoderView,
    #[serde(rename = "_artwork")]
    artwork: ArtworkView,
}

/// Look up cover art read-only, folding any error into a non-fatal `ArtworkView`.
fn artwork_view(input: &Path) -> ArtworkView {
    match lyra_library::artwork(input) {
        Ok(Some(a)) => {
            let source = if a.picture_type.is_some() {
                "embedded"
            } else {
                "folder"
            };
            ArtworkView {
                found: true,
                source: Some(source),
                mime: Some(a.mime),
                picture_type: a.picture_type,
                bytes: Some(a.data.len()),
                error: None,
            }
        }
        Ok(None) => ArtworkView {
            found: false,
            source: None,
            mime: None,
            picture_type: None,
            bytes: None,
            error: None,
        },
        Err(e) => ArtworkView {
            found: false,
            source: None,
            mime: None,
            picture_type: None,
            bytes: None,
            error: Some(e.to_string()),
        },
    }
}

/// Run `meta <file> [--json]`.
pub fn run(input: &Path, as_json: bool) -> Result<ExitCode> {
    let meta = read_meta(input)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&meta)?);
    } else {
        print_human(&meta);
    }
    Ok(ExitCode::SUCCESS)
}

/// Read the §6 field set for `input`, dispatching DSD files to the `lyra-dsd`
/// reader and everything else to `read_tags` + the PCM decoder probe.
fn read_meta(input: &Path) -> Result<Meta> {
    if is_dsd_ext(input) {
        read_dsd_meta(input)
    } else {
        read_pcm_meta(input)
    }
}

/// `.dsf`/`.dff` by extension — the two containers `read_tags` cannot parse.
fn is_dsd_ext(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("dsf" | "dff")
    )
}

/// PCM / lossy path: tags from `read_tags`, exact length from the decoder probe.
fn read_pcm_meta(input: &Path) -> Result<Meta> {
    let tm = lyra_library::read_tags(input)
        .with_context(|| format!("reading tags from {}", input.display()))?;

    // Exact length + authoritative format from the decoder, when a Lyra decoder
    // supports the container. MP3 (P1b) is not yet decodable, so this is best-
    // effort: a probe failure leaves `length_samples` null (never the approximate
    // tag-derived value) and records the gap in `_decoder`.
    let probe = lyra_decode::probe(input).ok();
    let (decoder, exact_len) = match probe {
        Some(p) => {
            let (rate, channels, bits) = match p.spec {
                StreamSpec::Pcm(s) => (Some(s.rate), Some(s.channels), Some(s.bits_valid)),
                StreamSpec::Dsd(_) => (None, None, None),
            };
            (
                DecoderView {
                    samplerate: rate,
                    channels,
                    bits,
                    total_frames: p.total_frames,
                },
                p.total_frames,
            )
        }
        None => (
            DecoderView {
                samplerate: None,
                channels: None,
                bits: None,
                total_frames: None,
            },
            None,
        ),
    };

    Ok(Meta {
        path: input.to_string_lossy().into_owned(),
        title: tm.title,
        artist: tm.artist,
        album: tm.album,
        album_artist: tm.album_artist,
        tracknumber: tm.track_number,
        discnumber: tm.disc_number,
        date: tm.date,
        genre: tm.genre,
        length_samples: exact_len,
        samplerate: tm.sample_rate,
        bitspersample: tm.bits,
        channels: tm.channels,
        codec: tm.codec,
        replaygain_track_gain: tm.replaygain_track_gain,
        replaygain_album_gain: tm.replaygain_album_gain,
        source: "pcm",
        duration_frames_approx: tm.duration_frames,
        decoder,
        artwork: artwork_view(input),
    })
}

/// DSD path: `read_tags` fails on `.dsf`/`.dff`, so every format field comes from
/// the `lyra-dsd` reader. DSD carries no Vorbis/ID3 text here, so the tag columns
/// are `null` (ID3 inside DSF is a later nicety; documented as a P6 gap).
fn read_dsd_meta(input: &Path) -> Result<Meta> {
    let info = lyra_dsd::probe(input)
        .with_context(|| format!("probing DSD file {}", input.display()))?;
    let rate_hz = info.spec.rate.hz();
    let channels = info.spec.channels;
    let frames = info.total_frames;

    Ok(Meta {
        path: input.to_string_lossy().into_owned(),
        title: None,
        artist: None,
        album: None,
        album_artist: None,
        tracknumber: None,
        discnumber: None,
        date: None,
        genre: None,
        length_samples: Some(frames),
        samplerate: Some(rate_hz),
        bitspersample: Some(1),
        channels: Some(channels),
        codec: Some("DSD".to_string()),
        replaygain_track_gain: None,
        replaygain_album_gain: None,
        source: "dsd",
        duration_frames_approx: Some(frames),
        decoder: DecoderView {
            samplerate: Some(rate_hz),
            channels: Some(channels),
            bits: Some(1),
            total_frames: Some(frames),
        },
        artwork: artwork_view(input),
    })
}

// (JSON access for in-process tests is covered by the end-to-end CLI test in
// `tests/meta.rs`, which drives the built binary exactly as the harness does.)

/// Pretty-print the §6 fields as aligned `key: value` lines (human mode).
fn print_human(m: &Meta) {
    fn s(o: &Option<String>) -> String {
        o.clone().unwrap_or_else(|| "<none>".to_string())
    }
    fn n<T: std::fmt::Display>(o: Option<T>) -> String {
        o.map(|v| v.to_string()).unwrap_or_else(|| "<none>".to_string())
    }
    println!("path:                  {}", m.path);
    println!("title:                 {}", s(&m.title));
    println!("artist:                {}", s(&m.artist));
    println!("album:                 {}", s(&m.album));
    println!("album_artist:          {}", s(&m.album_artist));
    println!("tracknumber:           {}", s(&m.tracknumber));
    println!("discnumber:            {}", s(&m.discnumber));
    println!("date:                  {}", s(&m.date));
    println!("genre:                 {}", s(&m.genre));
    println!("length_samples:        {}", n(m.length_samples));
    println!("samplerate:            {}", n(m.samplerate));
    println!("bitspersample:         {}", n(m.bitspersample));
    println!("channels:              {}", n(m.channels));
    println!("codec:                 {}", s(&m.codec));
    println!("replaygain_track_gain: {}", s(&m.replaygain_track_gain));
    println!("replaygain_album_gain: {}", s(&m.replaygain_album_gain));
    let art = if m.artwork.found {
        format!(
            "found ({}, {})",
            m.artwork.source.unwrap_or("?"),
            m.artwork.mime.as_deref().unwrap_or("?")
        )
    } else {
        "none".to_string()
    };
    println!("artwork:               {art}");
    println!("(source: {}, approx length {})", m.source, n(m.duration_frames_approx));
}

