//! `lyra-play` — the minimal headless player for P2 (S04/S05).
//!
//! It plays one raw interleaved PCM file through [`lyra_output`] in WASAPI
//! exclusive mode (bit-perfect) or shared mode, padding the source into the
//! negotiated device container off the real-time thread, and can mirror the exact
//! device bytes to a tap file (`--tap`, the bit-perfect proof, ADR-004).
//!
//! It has no queue, no decoder and no gapless logic — those arrive with the
//! playback engine in P3. The input is *raw PCM*; a decoder front-end (e.g.
//! `lyra-probe decode` feeding this) produces it.
//!
//! # Sound rule
//! Rendering makes noise. On the dev laptop every render must go through QA's mute
//! helper and stay short; `--max-seconds` (default 30) bounds a run. `--probe`
//! only queries the driver (`IsFormatSupported`) and never renders, so it is safe
//! to run unattended.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use lyra_core::{OutputMode, PcmSpec, SampleFormat, StreamSpec};
use lyra_output::{DeviceId, OutputOptions};

/// Headless raw-PCM player and capability probe.
#[derive(Parser, Debug)]
#[command(name = "lyra-play", version, about, long_about = None)]
struct Cli {
    /// List render devices with their exclusive capabilities, then exit. No input
    /// file is needed and no audio is rendered.
    #[arg(long)]
    probe: bool,

    /// Raw interleaved PCM input file (required unless --probe).
    input: Option<PathBuf>,

    /// Endpoint id (from --probe). Omit for the system default render endpoint.
    #[arg(long)]
    device: Option<String>,

    /// Source sample rate in Hz (required to play).
    #[arg(long)]
    rate: Option<u32>,

    /// Channel count.
    #[arg(long, default_value_t = 2)]
    channels: u16,

    /// Meaningful (valid) bits per sample in the source.
    #[arg(long, default_value_t = 16)]
    bits: u8,

    /// Container word size of the input samples.
    #[arg(long, value_enum, default_value_t = Fmt::S16)]
    format: Fmt,

    /// Use WASAPI shared mode (flagged NOT bit-perfect) instead of exclusive.
    #[arg(long)]
    shared: bool,

    /// Allow the push-mode (timer) fallback if the event handle cannot be set up.
    #[arg(long)]
    poll: bool,

    /// Mirror the exact device bytes to this file (the device tap, ADR-004).
    #[arg(long)]
    tap: Option<PathBuf>,

    /// Stop after this many seconds (safety cap for the sound rule).
    #[arg(long, default_value_t = 30)]
    max_seconds: u64,
}

/// Container format of the raw input.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Fmt {
    S16,
    S24,
    S32,
}

impl Fmt {
    fn to_sample_format(self) -> SampleFormat {
        match self {
            Fmt::S16 => SampleFormat::S16,
            Fmt::S24 => SampleFormat::S24,
            Fmt::S32 => SampleFormat::S32,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = if cli.probe { run_probe(&cli) } else { run_play(&cli) };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("lyra-play: {msg}");
            ExitCode::from(3)
        }
    }
}

/// `--probe`: enumerate devices and print their capabilities. Degrades to a clean
/// message (and success) when no endpoint is available.
fn run_probe(cli: &Cli) -> Result<(), String> {
    match &cli.device {
        // Probe a single endpoint.
        Some(id) => {
            let probe = lyra_output::probe(&DeviceId::Endpoint(id.clone()))
                .map_err(|e| format!("probe failed: {e}"))?;
            print_probe(&probe);
        }
        // Probe every render endpoint.
        None => {
            let devices = match lyra_output::devices() {
                Ok(d) => d,
                Err(e) => {
                    println!("No render devices available ({e}).");
                    return Ok(());
                }
            };
            if devices.is_empty() {
                println!("No render devices found.");
                return Ok(());
            }
            for dev in &devices {
                match lyra_output::probe(&DeviceId::Endpoint(dev.id.clone())) {
                    Ok(p) => print_probe(&p),
                    Err(e) => println!("{} [{}]: probe failed: {e}", dev.name, dev.id),
                }
                println!();
            }
        }
    }
    Ok(())
}

fn print_probe(p: &lyra_output::DeviceProbe) {
    println!(
        "Device: {}{}",
        p.info.name,
        if p.info.is_default { "  (default)" } else { "" }
    );
    println!("  id: {}", p.info.id);
    match p.shared_mix_rate {
        Some(r) => println!("  shared mix rate: {r} Hz"),
        None => println!("  shared mix rate: (unavailable)"),
    }
    if p.exclusive_formats.is_empty() {
        println!("  exclusive formats: none accepted");
    } else {
        println!("  exclusive formats (rate / container / valid-bits):");
        for f in &p.exclusive_formats {
            println!(
                "    {:>7} Hz  {:<4} {}-bit valid",
                f.rate,
                format!("{:?}", f.format),
                f.valid_bits
            );
        }
    }
    println!("  DoP capable: {}  native DSD rates: {}", p.dop_capable, p.native_dsd_rates.len());
}

/// Play one raw-PCM file.
fn run_play(cli: &Cli) -> Result<(), String> {
    let input = cli.input.as_ref().ok_or("an input file is required (or use --probe)")?;
    let rate = cli.rate.ok_or("--rate is required to play")?;
    let source = PcmSpec {
        rate,
        channels: cli.channels,
        bits_valid: cli.bits,
        format: cli.format.to_sample_format(),
    };

    let dev = match &cli.device {
        Some(id) => DeviceId::Endpoint(id.clone()),
        None => DeviceId::Default,
    };
    let mode = if cli.shared { OutputMode::WasapiShared } else { OutputMode::WasapiExclusive };
    let opts = OutputOptions { tap_path: cli.tap.clone(), allow_poll_fallback: cli.poll };

    let mut handle = lyra_output::open(&dev, &StreamSpec::Pcm(source), mode, opts)
        .map_err(|e| format!("open device: {e}"))?;

    let negotiated = match handle.negotiated() {
        StreamSpec::Pcm(p) => p,
        StreamSpec::Dsd(_) => return Err("device negotiated a DSD stream unexpectedly".into()),
    };
    println!(
        "Playing {input:?}\n  source:     {} Hz, {} ch, {}-bit valid, {:?}",
        source.rate, source.channels, source.bits_valid, source.format
    );
    println!(
        "  negotiated: {} Hz, {} ch, {:?} container  [{}]",
        negotiated.rate,
        negotiated.channels,
        negotiated.format,
        if handle.is_bit_perfect() { "bit-perfect" } else { "NOT bit-perfect" }
    );

    let frames_pushed = feed_file(&mut handle, input, source, negotiated, cli.max_seconds)?;

    // Wait for the device to play out what we pushed (plus a little for the
    // priming buffer), then stop. The tap, if any, keeps the exact bytes.
    wait_drain(&handle, frames_pushed);

    let stats = handle.stats();
    println!(
        "Done. frames played: {}, underruns: {}, device lost: {}",
        stats.frames_played, stats.underruns, stats.device_lost
    );
    if let Some(dropped) = handle.tap_dropped() {
        println!("Tap dropped bytes: {dropped} (should be 0)");
    }
    handle.stop();
    Ok(())
}

/// Read the raw file frame by frame, pad into the device container off the render
/// thread, and push into the ring with simple back-pressure. Returns the number
/// of source frames pushed.
fn feed_file(
    handle: &mut lyra_output::OutputHandle,
    input: &PathBuf,
    source: PcmSpec,
    negotiated: PcmSpec,
    max_seconds: u64,
) -> Result<u64, String> {
    let mut reader = BufReader::new(File::open(input).map_err(|e| format!("open input: {e}"))?);
    let src_frame = source.bytes_per_frame();
    let max_frames = rate_cap(source.rate, max_seconds);

    let chunk_frames = 8192usize;
    let mut inbuf = vec![0u8; chunk_frames * src_frame];
    let mut carry = 0usize; // leftover partial-frame bytes from the last read
    let mut padded: Vec<u8> = Vec::new();
    let mut frames_pushed: u64 = 0;

    loop {
        if frames_pushed >= max_frames {
            println!("Reached --max-seconds cap ({max_seconds}s); stopping.");
            break;
        }
        let n = reader.read(&mut inbuf[carry..]).map_err(|e| format!("read input: {e}"))?;
        if n == 0 && carry == 0 {
            break; // EOF, nothing buffered
        }
        let avail = carry + n;
        let whole = (avail / src_frame) * src_frame;
        if whole == 0 {
            if n == 0 {
                break; // trailing partial frame at EOF: drop it
            }
            carry = avail;
            continue;
        }

        // Pad whole frames into the device container (off the RT thread).
        lyra_output::pad_frames(source, negotiated.format, &inbuf[..whole], &mut padded);
        push_all(handle, &padded)?;
        frames_pushed += (whole / src_frame) as u64;

        // Move any partial-frame tail to the front for the next read.
        carry = avail - whole;
        inbuf.copy_within(whole..avail, 0);

        if n == 0 {
            break;
        }
    }
    Ok(frames_pushed)
}

/// Push a whole slice into the ring, waiting (off the RT thread) when it is full.
fn push_all(handle: &mut lyra_output::OutputHandle, mut bytes: &[u8]) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !bytes.is_empty() {
        let (_written, remainder) = handle.producer().push_partial_slice(bytes);
        bytes = remainder;
        if !bytes.is_empty() {
            if handle.stats().device_lost {
                return Err("device lost while feeding".into());
            }
            if Instant::now() > deadline {
                return Err("ring stayed full for 10s (device not draining)".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}

/// Spin until the device has played roughly all pushed frames, with a timeout.
fn wait_drain(handle: &lyra_output::OutputHandle, target_frames: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let s = handle.stats();
        if s.device_lost || s.frames_played >= target_frames {
            break;
        }
        if Instant::now() > deadline {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // A little extra for the priming buffer to finish.
    thread::sleep(Duration::from_millis(150));
}

/// Frame cap for a duration, saturating.
fn rate_cap(rate: u32, seconds: u64) -> u64 {
    u64::from(rate).saturating_mul(seconds)
}
