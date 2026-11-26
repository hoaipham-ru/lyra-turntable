//! S09 tool — `lyra-probe`: the QA probe CLI for foobar2000 parity testing.
//!
//! Subcommands:
//! - `decode`  — decode an audio file to raw/WAV PCM and print a SHA-256 of the
//!   raw payload. Defaults to the merged `lyra-decode` engine (P1.5); `--engine
//!   ffmpeg` selects the P0 reference path (ADR-007).
//! - `compare` — the core QA primitive: bit-exact / tolerance comparison of two
//!   PCM streams, with first differing frame, max abs diff and null depth (dB).
//!   Audio files on either side are decoded with `--engine` (default `lyra`).
//! - `mute`    — endpoint mute control (`on|off|report`) for the Sound rule; uses
//!   the endpoint volume API and never touches the digital stream (P2).
//! - `align`, `capture`, `meta` — stubs; implemented in later phases.
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
mod dsd;
mod dsd_hash;
mod engine;
mod ffmpeg;
mod meta;
mod mute;
mod pcm;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use mute::MuteAction;
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
    /// Dump a file's foobar2000-parity metadata (TEST-STRATEGY §6) as text/JSON.
    Meta(MetaArgs),
    /// Control the endpoint mute flag for the Sound rule (does not touch samples).
    Mute(MuteArgs),
    /// [P5] DSD helpers: convert DFF -> DSF fixtures (writes under target/ only).
    Dsd(DsdArgs),
}

#[derive(Parser)]
struct MetaArgs {
    /// Input audio file (FLAC/WAV/MP3/… or a DSF/DFF DSD file).
    input: PathBuf,
    /// Emit a JSON object keyed by the TEST-STRATEGY §6 columns (plus `_`-prefixed
    /// diagnostics) for the comparison harness. Omit for human-readable text.
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct DsdArgs {
    #[command(subcommand)]
    command: DsdCommands,
}

#[derive(Subcommand)]
enum DsdCommands {
    /// Re-container an uncompressed DFF as DSF (identical DSD, bit order flipped).
    Convert(ConvertArgs),
    /// Hash a DSF/DFF file's DSD bytes (native + MSB-first canonical) for parity.
    Hash(HashArgs),
}

#[derive(Parser)]
struct HashArgs {
    /// Input DSF or DFF file.
    input: PathBuf,
}

#[derive(Parser)]
struct ConvertArgs {
    /// Input DFF (DSDIFF) file.
    input: PathBuf,
    /// Output DSF path. Must be under a `target/` directory (ADR-006). Omit to use
    /// `target/corpus-synth/dsf/<input-stem>.dsf`.
    output: Option<PathBuf>,
}

/// Decoder backend for audio inputs.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Engine {
    /// The merged `lyra-decode` crate — the decoder under test (P1.5, default).
    Lyra,
    /// The ffmpeg reference path (P0 / ADR-007 third opinion).
    Ffmpeg,
}

#[derive(Parser)]
struct MuteArgs {
    /// What to do with the endpoint mute flag.
    #[arg(value_enum)]
    action: MuteActionArg,
    /// Target render endpoint id. Omit for the default-console endpoint.
    #[arg(long)]
    device: Option<String>,
}

/// CLI spelling of [`MuteAction`].
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum MuteActionArg {
    /// Mute the endpoint.
    On,
    /// Unmute the endpoint.
    Off,
    /// Report the current mute state without changing it.
    Report,
}

impl From<MuteActionArg> for MuteAction {
    fn from(a: MuteActionArg) -> Self {
        match a {
            MuteActionArg::On => MuteAction::On,
            MuteActionArg::Off => MuteAction::Off,
            MuteActionArg::Report => MuteAction::Report,
        }
    }
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
    /// Decoder backend: `lyra` (the engine under test, default) or `ffmpeg`.
    #[arg(long, value_enum, default_value_t = Engine::Lyra)]
    engine: Engine,
    /// Seek to this source frame before decoding (lyra engine only). The hash and
    /// `--out` then cover the post-seek output, for seek-parity against a golden
    /// slice (TEST-STRATEGY §4).
    #[arg(long)]
    seek: Option<u64>,
    /// Stop after this many frames (useful with `--seek` to grab a fixed slice).
    #[arg(long)]
    frames: Option<u64>,
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
    /// Per-side overrides for the B stream, so a device tap in one container (e.g.
    /// 24-in-32) can be compared against a packed-24 golden. Each defaults to the
    /// A value. The ADR-004 shift then makes the two compare directly.
    #[arg(long)]
    bits_b: Option<u32>,
    /// Container format override for a raw B input (defaults to `--raw-format`).
    #[arg(long, value_enum)]
    raw_format_b: Option<RawFormat>,
    /// Channel count override for a raw B input (defaults to `--channels`).
    #[arg(long)]
    channels_b: Option<u16>,
    /// Decoder backend for audio-file inputs: `lyra` (default) or `ffmpeg`. Raw
    /// inputs are read directly and ignore this.
    #[arg(long, value_enum, default_value_t = Engine::Lyra)]
    engine: Engine,
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
        Commands::Meta(a) => meta::run(&a.input, a.json),
        Commands::Mute(a) => mute::run(a.action.into(), a.device.as_deref()),
        Commands::Dsd(a) => cmd_dsd(&a),
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

fn cmd_dsd(args: &DsdArgs) -> Result<ExitCode> {
    match &args.command {
        DsdCommands::Convert(c) => {
            let output = c
                .output
                .clone()
                .unwrap_or_else(|| dsd::default_fixture_path(&c.input));
            dsd::run_convert(&c.input, &output)
        }
        DsdCommands::Hash(h) => dsd_hash::run_hash(&h.input),
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

/// Decode an audio file to raw interleaved PCM using the selected backend.
fn decode_with(engine: Engine, path: &Path) -> Result<ffmpeg::Decoded> {
    match engine {
        Engine::Lyra => engine::decode_raw(path),
        Engine::Ffmpeg => ffmpeg::decode_raw(path),
    }
}

fn cmd_decode(args: &DecodeArgs) -> Result<ExitCode> {
    let sliced = args.seek.is_some() || args.frames.is_some();
    if sliced && args.engine != Engine::Lyra {
        bail!("--seek/--frames are only supported with --engine lyra");
    }
    let decoded = if sliced {
        engine::decode_slice(&args.input, args.seek, args.frames)?
    } else {
        decode_with(args.engine, &args.input)?
    };
    let spec = decoded.spec;
    let hash = sha256_hex(&decoded.pcm);

    // For the engine under test, also report the container-declared length so a
    // harness can assert `total_frames == decoded frames` (sample-accurate length,
    // == foobar2000 %length_samples% for the lossless corpus). ffmpeg has no such
    // field here, so it is reported null.
    let declared_frames: Option<u64> = match args.engine {
        Engine::Lyra => lyra_decode::probe(&args.input).ok().and_then(|p| p.total_frames),
        Engine::Ffmpeg => None,
    };

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
        "engine": match args.engine { Engine::Lyra => "lyra", Engine::Ffmpeg => "ffmpeg" },
        "rate": spec.rate,
        "channels": spec.channels,
        "bits": spec.bits_valid,
        "format": format_name(spec.format),
        "pcmBytes": decoded.pcm.len(),
        "pcmFrames": decoded.pcm.len() / spec.bytes_per_frame(),
        "declaredFrames": declared_frames,
        "seek": args.seek,
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

/// Per-side raw-input parameters (an audio file ignores these and uses its spec).
struct RawParams {
    raw_format: Option<RawFormat>,
    bits: Option<u32>,
    channels: Option<u16>,
}

fn load_stream(path: &Path, p: &RawParams, engine: Engine) -> Result<Loaded> {
    if is_raw_ext(path) {
        let fmt = p
            .raw_format
            .map(RawFormat::to_sample_format)
            .context("raw input needs --raw-format (s16/s24/s32)")?;
        let bits = p.bits.unwrap_or(match fmt {
            SampleFormat::S16 => 16,
            SampleFormat::S24 => 24,
            SampleFormat::S32 | SampleFormat::F32 => 32,
        });
        let channels = p.channels.context("raw input needs --channels")?;
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Loaded {
            samples: pcm::unpack(&bytes, fmt, bits)?,
            bits,
            channels,
            source: format!("{} ({}, {}-bit, {}ch)", path.display(), format_name(fmt), bits, channels),
        })
    } else {
        let decoded = decode_with(engine, path)?;
        let spec: PcmSpec = decoded.spec;
        let bits = p.bits.unwrap_or(u32::from(spec.bits_valid));
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
    let a_params = RawParams {
        raw_format: args.raw_format,
        bits: args.bits,
        channels: args.channels,
    };
    // B defaults to A's parameters; the *_b overrides let a tap (e.g. 24-in-32) be
    // compared against a packed-24 golden.
    let b_params = RawParams {
        raw_format: args.raw_format_b.or(args.raw_format),
        bits: args.bits_b.or(args.bits),
        channels: args.channels_b.or(args.channels),
    };
    let mut a = load_stream(&args.a, &a_params, args.engine)?;
    let mut b = load_stream(&args.b, &b_params, args.engine)?;

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
