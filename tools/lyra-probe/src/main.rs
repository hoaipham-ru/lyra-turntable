//! S09 tool — `lyra-probe`: the QA probe CLI for foobar2000 parity testing.
//!
//! Subcommands:
//! - `decode`  — decode an audio file to raw/WAV PCM and print a SHA-256 of the
//!   raw payload (ffmpeg-backed reference decode for P0; ADR-007).
//! - `compare` — the core QA primitive: bit-exact / tolerance comparison of two
//!   PCM streams, with first differing frame, max abs diff and null depth (dB).
//! - `align`, `capture`, `meta`, `mute` — stubs; implemented in later phases.
//!
//! # Exit-code contract
//! Gating scripts and CI depend on these codes being distinct:
//! - `0` — success: `decode` ran, or `compare` found the streams within tolerance.
//! - `1` — `compare` mismatch: the streams differ beyond the chosen tolerance.
//! - `2` — stub / not-implemented subcommand.
//! - `3` — harness or internal error (ffmpeg/ffprobe missing, decode failure,
//!   unsupported format, I/O error, …). The streams were *not* compared.
//!
//! Reserving `1` for a genuine compare mismatch lets a caller tell "streams
//! differ" apart from "the probe broke".
//!
//! See `testing/TEST-STRATEGY.md` §4 (parity criteria) and §9 (report format).

mod compare;
mod ffmpeg;
mod pcm;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use lyra_core::{PcmSpec, SampleFormat};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Lyra QA probe — decode, hash and compare PCM against golden references.
#[derive(Parser)]
#[command(name = "lyra-probe", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Decode an audio file to raw/WAV PCM and print its spec + SHA-256 (JSON).
    Decode(DecodeArgs),
    /// Compare two PCM streams (raw or audio files decoded on the fly).
    Compare(CompareArgs),
    /// [P2] Cross-correlation alignment of two captures.
    Align,
    /// [P2] Capture device-boundary bytes via the S04 helper.
    Capture,
    /// [P0.6] Dump a file's TrackMeta as JSON.
    Meta,
    /// [P2] Mute/silence verification.
    Mute,
}

/// Output container for `decode`.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum OutFormat {
    /// Headerless interleaved PCM.
    Raw,
    /// Canonical 44-byte WAV wrapper around the same PCM.
    Wav,
}

#[derive(Parser)]
struct DecodeArgs {
    /// Input audio file.
    input: PathBuf,
    /// Write the decoded PCM (or WAV) here. Omit to only hash.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Output container when `--out` is given.
    #[arg(long, value_enum, default_value_t = OutFormat::Raw)]
    format: OutFormat,
    /// Print the SHA-256 of the raw PCM payload (always printed in JSON too).
    #[arg(long)]
    hash: bool,
}

#[derive(Parser)]
struct CompareArgs {
    /// First stream: an audio file, or a raw/.pcm file (needs `--format`/`--bits`/`--channels`).
    a: PathBuf,
    /// Second stream.
    b: PathBuf,
    /// Maximum per-sample absolute difference (LSBs) still treated as a match.
    #[arg(long, default_value_t = 0)]
    tolerance: i64,
    /// Valid bit depth to compare at. Drives the ADR-004 shift when one side is
    /// e.g. 24-in-32 and the other packed 24-bit. Defaults to each side's own.
    #[arg(long)]
    bits: Option<u32>,
    /// Container format for raw inputs (s16/s24/s32). Ignored for audio files.
    #[arg(long, value_enum)]
    raw_format: Option<RawFormat>,
    /// Channel count for raw inputs.
    #[arg(long)]
    channels: Option<u16>,
}

/// Container format selector for raw PCM inputs.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum RawFormat {
    /// signed 16-bit little-endian
    S16,
    /// signed 24-bit packed little-endian
    S24,
    /// signed 32-bit little-endian
    S32,
}

impl RawFormat {
    fn to_sample_format(self) -> SampleFormat {
        match self {
            RawFormat::S16 => SampleFormat::S16,
            RawFormat::S24 => SampleFormat::S24,
            RawFormat::S32 => SampleFormat::S32,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let r = match cli.command {
        Commands::Decode(a) => cmd_decode(&a),
        Commands::Compare(a) => cmd_compare(&a),
        Commands::Align => stub("align", "P2"),
        Commands::Capture => stub("capture", "P2"),
        Commands::Meta => stub("meta", "P0.6"),
        Commands::Mute => stub("mute", "P2"),
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("lyra-probe error: {e:#}");
            // Exit code 3: a harness/internal error, distinct from a compare
            // mismatch (1) so gating scripts can tell "the probe broke" apart
            // from "the streams differ". See the module exit-code contract.
            ExitCode::from(3)
        }
    }
}

fn stub(name: &str, phase: &str) -> Result<ExitCode> {
    eprintln!("lyra-probe: `{name}` not implemented until {phase}");
    Ok(ExitCode::from(2))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn format_name(f: SampleFormat) -> &'static str {
    match f {
        SampleFormat::S16 => "s16le",
        SampleFormat::S24 => "s24le",
        SampleFormat::S32 => "s32le",
        SampleFormat::F32 => "f32le",
    }
}

fn cmd_decode(args: &DecodeArgs) -> Result<ExitCode> {
    let decoded = ffmpeg::decode_raw(&args.input)?;
    let spec = decoded.spec;
    let hash = sha256_hex(&decoded.pcm);

    if let Some(out) = &args.out {
        let bytes = match args.format {
            OutFormat::Raw => decoded.pcm.clone(),
            OutFormat::Wav => {
                let mut w = ffmpeg::wav_header(&spec, decoded.pcm.len())?;
                w.extend_from_slice(&decoded.pcm);
                w
            }
        };
        std::fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;
    }

    let report = serde_json::json!({
        "input": args.input.to_string_lossy(),
        "rate": spec.rate,
        "channels": spec.channels,
        "bits": spec.bits_valid,
        "format": format_name(spec.format),
        "pcmBytes": decoded.pcm.len(),
        "pcmFrames": decoded.pcm.len() / spec.bytes_per_frame(),
        "sha256": hash,
        "out": args.out.as_ref().map(|p| p.to_string_lossy()),
        "outFormat": args.out.as_ref().map(|_| match args.format {
            OutFormat::Raw => "raw",
            OutFormat::Wav => "wav",
        }),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    let _ = args.hash; // hash always included in JSON; flag kept for scripts
    Ok(ExitCode::SUCCESS)
}

/// A loaded stream: valid-bit-justified samples plus the bits/channels used.
struct Loaded {
    samples: Vec<i32>,
    bits: u32,
    channels: u16,
    source: String,
}

fn is_raw_ext(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("raw" | "pcm" | "bin")
    )
}

fn load_stream(path: &Path, args: &CompareArgs) -> Result<Loaded> {
    if is_raw_ext(path) {
        let fmt = args
            .raw_format
            .map(RawFormat::to_sample_format)
            .context("raw input needs --raw-format (s16/s24/s32)")?;
        let bits = args.bits.unwrap_or(match fmt {
            SampleFormat::S16 => 16,
            SampleFormat::S24 => 24,
            SampleFormat::S32 | SampleFormat::F32 => 32,
        });
        let channels = args.channels.context("raw input needs --channels")?;
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Loaded {
            samples: pcm::unpack(&bytes, fmt, bits)?,
            bits,
            channels,
            source: format!("{} ({}, {}-bit, {}ch)", path.display(), format_name(fmt), bits, channels),
        })
    } else {
        let decoded = ffmpeg::decode_raw(path)?;
        let spec: PcmSpec = decoded.spec;
        let bits = args.bits.unwrap_or(u32::from(spec.bits_valid));
        Ok(Loaded {
            samples: pcm::unpack(&decoded.pcm, spec.format, bits)?,
            bits,
            channels: spec.channels,
            source: format!(
                "{} ({}, {}-bit valid, {}ch, {} Hz)",
                path.display(),
                format_name(spec.format),
                spec.bits_valid,
                spec.channels,
                spec.rate
            ),
        })
    }
}

fn cmd_compare(args: &CompareArgs) -> Result<ExitCode> {
    let mut a = load_stream(&args.a, args)?;
    let mut b = load_stream(&args.b, args)?;

    if a.channels != b.channels {
        bail!(
            "channel count differs: A has {} ch, B has {} ch",
            a.channels,
            b.channels
        );
    }

    // Align scales: lift the lower-bit side up so both sit at the same LSB.
    let common_bits = a.bits.max(b.bits);
    if a.bits < common_bits {
        let sh = common_bits - a.bits;
        for s in &mut a.samples {
            *s <<= sh;
        }
    }
    if b.bits < common_bits {
        let sh = common_bits - b.bits;
        for s in &mut b.samples {
            *s <<= sh;
        }
    }

    let result = compare::compare_pcm(
        &a.samples,
        &b.samples,
        usize::from(a.channels),
        common_bits,
        args.tolerance,
    );

    let report = serde_json::json!({
        "a": a.source,
        "b": b.source,
        "compareBits": common_bits,
        "result": result,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);

    // Exit-code contract: 0 = match, 1 = genuine mismatch (streams differ beyond
    // tolerance). Harness errors never reach here — they exit 3 via main's Err
    // arm — so a caller can distinguish "streams differ" from "the probe broke".
    if result.within_tolerance {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}
