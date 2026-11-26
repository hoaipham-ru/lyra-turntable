//! `lyra-play` — the headless Lyra player (S04/S05).
//!
//! Two front-ends share one binary:
//!
//! * **Engine queue mode** (default, P3): one or more decodable files become a
//!   `SetQueue` played gaplessly through [`lyra_engine::Engine`]. `--tap` mirrors
//!   the exact device bytes (the bit-perfect proof, ADR-004); `--offline` renders
//!   to a file with no audio device at all (headless, silent, deterministic — the
//!   preferred unattended path for QA). `--script` feeds a line-oriented command
//!   script so gapless, seek and command-storm scenarios run deterministically.
//! * **Raw-PCM mode** (legacy P2): selected by passing `--rate`, it plays one raw
//!   interleaved PCM file straight through [`lyra_output`], exactly as in P2.
//!
//! `--probe` only queries the driver (`IsFormatSupported`) and never renders.
//!
//! # Sound rule
//! Rendering makes noise. On the dev laptop every render must go through QA's mute
//! helper and stay short; `--max-seconds` (default 30) bounds a run. `--offline`
//! and `--probe` never open a device, so they are safe to run unattended.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use crossbeam_channel::Receiver;
use lyra_core::{OutputMode, PcmSpec, SampleFormat, StreamSpec};
use lyra_engine::{
    Engine, EngineCommand, EngineConfig, EngineEvent, EngineHandle, PlayState, TrackRef,
};
use lyra_output::{DeviceId, OutputOptions};

mod script;
use script::{parse_script, ScriptCmd};

/// The engine event receiver.
type Events = Receiver<EngineEvent>;

/// Headless player, queue driver and capability probe.
#[derive(Parser, Debug)]
#[command(name = "lyra-play", version, about, long_about = None)]
struct Cli {
    /// List render devices with their exclusive capabilities, then exit. No input
    /// is needed and no audio is rendered.
    #[arg(long)]
    probe: bool,

    /// Input files. One or more decodable files form a gapless queue (engine mode);
    /// a `.cue` expands to all its virtual tracks. In raw-PCM mode (`--rate`) only
    /// the first file is used.
    inputs: Vec<PathBuf>,

    /// Endpoint id (from --probe). Omit for the system default render endpoint.
    #[arg(long)]
    device: Option<String>,

    /// Output mode for engine playback.
    #[arg(long, value_enum, default_value_t = Mode::Exclusive)]
    mode: Mode,

    /// Mirror the exact device bytes to this file (the device tap, ADR-004).
    #[arg(long)]
    tap: Option<PathBuf>,

    /// Render to this file with no audio device opened (headless, silent). The
    /// bytes equal what a device tap would record.
    #[arg(long)]
    offline: Option<PathBuf>,

    /// Feed a line-oriented command script instead of just playing to the end.
    #[arg(long)]
    script: Option<PathBuf>,

    /// Stop after this many seconds (safety cap for the sound rule).
    #[arg(long, default_value_t = 30)]
    max_seconds: u64,

    /// Allow the push-mode (timer) fallback if the event handle cannot be set up.
    #[arg(long)]
    poll: bool,

    // --- Raw-PCM (legacy P2) mode: selected when --rate is given ---
    /// Source sample rate in Hz. Its presence selects raw-PCM mode.
    #[arg(long)]
    rate: Option<u32>,

    /// Channel count (raw-PCM mode).
    #[arg(long, default_value_t = 2)]
    channels: u16,

    /// Meaningful (valid) bits per sample in the source (raw-PCM mode).
    #[arg(long, default_value_t = 16)]
    bits: u8,

    /// Container word size of the input samples (raw-PCM mode).
    #[arg(long, value_enum, default_value_t = Fmt::S16)]
    format: Fmt,

    /// Use WASAPI shared mode in raw-PCM mode (flagged NOT bit-perfect).
    #[arg(long)]
    shared: bool,
}

/// Engine output mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Mode {
    Exclusive,
    Shared,
    Asio,
}

impl Mode {
    fn to_output_mode(self) -> OutputMode {
        match self {
            Mode::Exclusive => OutputMode::WasapiExclusive,
            Mode::Shared => OutputMode::WasapiShared,
            Mode::Asio => OutputMode::Asio { native_dsd: false },
        }
    }
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
    let result = if cli.probe {
        run_probe(&cli)
    } else if cli.rate.is_some() {
        run_raw(&cli)
    } else {
        run_engine(&cli)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("lyra-play: {msg}");
            ExitCode::from(3)
        }
    }
}

// --- Engine queue mode -----------------------------------------------------

/// Drive the playback engine with a queue and (optionally) a command script.
fn run_engine(cli: &Cli) -> Result<(), String> {
    if cli.inputs.is_empty() {
        return Err("at least one input file is required (or use --probe)".into());
    }
    let device = match &cli.device {
        Some(id) => DeviceId::Endpoint(id.clone()),
        None => DeviceId::Default,
    };
    let config = EngineConfig {
        device,
        mode: cli.mode.to_output_mode(),
        tap_path: cli.tap.clone(),
        allow_poll_fallback: cli.poll,
        offline: cli.offline.clone(),
        ..EngineConfig::default()
    };

    let queue: Vec<TrackRef> = cli
        .inputs
        .iter()
        .map(|p| TrackRef::file(p.to_string_lossy().into_owned()))
        .collect();

    println!("Queue ({} entries):", queue.len());
    for (i, t) in queue.iter().enumerate() {
        println!("  [{i}] {}", t.path);
    }
    match (&cli.offline, &cli.tap) {
        (Some(p), _) => println!("Offline render (no device) → {}", p.display()),
        (None, Some(p)) => println!("Rendering with device tap → {}", p.display()),
        (None, None) => println!("Rendering to the audio device (honour the sound rule)."),
    }

    let (engine, events) = Engine::new(config).map_err(|e| format!("engine start: {e}"))?;
    engine.send(EngineCommand::SetQueue(queue));

    let deadline = Instant::now() + Duration::from_secs(cli.max_seconds);
    let mut session = Session::new();

    match &cli.script {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("reading script {}: {e}", path.display()))?;
            let cmds = parse_script(&text).map_err(|e| format!("script: {e}"))?;
            session.run_script(&engine, &events, &cmds, deadline);
        }
        None => {
            engine.send(EngineCommand::Play);
            session.run_to_idle(&events, deadline);
        }
    }

    engine.shutdown();
    session.print_summary();
    Ok(())
}

/// Tracks event-printing state and run flags across a session.
struct Session {
    saw_playing: bool,
    stopped: bool,
    last_pos_second: i64,
    underruns: u64,
    errors: u64,
}

impl Session {
    fn new() -> Self {
        Self { saw_playing: false, stopped: false, last_pos_second: -1, underruns: 0, errors: 0 }
    }

    /// Play until the queue ends (Stopped after Playing) or the deadline.
    fn run_to_idle(&mut self, events: &Events, deadline: Instant) {
        while Instant::now() < deadline {
            if self.stopped && self.saw_playing {
                break;
            }
            self.drain(events, Duration::from_millis(100));
        }
    }

    /// Execute a parsed command script against the engine.
    fn run_script(
        &mut self,
        engine: &EngineHandle,
        events: &Events,
        cmds: &[ScriptCmd],
        deadline: Instant,
    ) {
        for cmd in cmds {
            if Instant::now() >= deadline {
                println!("[script] max-seconds reached; stopping.");
                break;
            }
            match cmd {
                ScriptCmd::Engine(ec) => {
                    println!("[script] {ec:?}");
                    engine.send(ec.clone());
                    self.drain(events, Duration::from_millis(20));
                }
                ScriptCmd::Wait(ms) => {
                    println!("[script] wait {ms}ms");
                    let until = (Instant::now() + Duration::from_millis(*ms)).min(deadline);
                    while Instant::now() < until {
                        self.drain(events, Duration::from_millis(20));
                    }
                }
                ScriptCmd::WaitEos => {
                    println!("[script] wait-eos");
                    // Return once the engine is idle after having played; if it has
                    // already ended, return at once (no hang).
                    while Instant::now() < deadline && !(self.stopped && self.saw_playing) {
                        self.drain(events, Duration::from_millis(50));
                    }
                }
                ScriptCmd::Quit => {
                    println!("[script] quit");
                    break;
                }
            }
        }
    }

    /// Drain and print any events available within `budget`.
    fn drain(&mut self, events: &Events, budget: Duration) {
        let end = Instant::now() + budget;
        loop {
            let now = Instant::now();
            if now >= end {
                return;
            }
            match events.recv_timeout(end - now) {
                Ok(ev) => self.print_event(ev),
                Err(_) => return,
            }
        }
    }

    fn print_event(&mut self, ev: EngineEvent) {
        match ev {
            EngineEvent::State(s) => {
                println!("State: {s:?}");
                if s == PlayState::Playing {
                    self.saw_playing = true;
                }
                if s == PlayState::Stopped {
                    self.stopped = true;
                }
            }
            EngineEvent::Track(info) => {
                println!(
                    "Track: {}{}  [{} frames]",
                    info.track.path,
                    info.title.map(|t| format!(" — {t}")).unwrap_or_default(),
                    info.duration_frames.map(|f| f.to_string()).unwrap_or_else(|| "?".into())
                );
            }
            EngineEvent::Format { source, device, bit_perfect } => {
                println!(
                    "Format: source {} → device {}  [{}]",
                    describe_spec(source),
                    describe_spec(device),
                    if bit_perfect { "bit-perfect" } else { "NOT bit-perfect" }
                );
            }
            EngineEvent::Position { frame } => {
                // Summarise: one line per whole second of position.
                let second = (frame / 48_000) as i64;
                if second != self.last_pos_second {
                    self.last_pos_second = second;
                    println!("Position: {frame} frames");
                }
            }
            EngineEvent::Underrun { count } => {
                self.underruns = count;
                println!("Underrun: {count}");
            }
            EngineEvent::Error(e) => {
                self.errors += 1;
                println!("Error: {e}");
            }
        }
    }

    fn print_summary(&self) {
        println!("Done. underruns: {}, errors: {}", self.underruns, self.errors);
    }
}

fn describe_spec(spec: StreamSpec) -> String {
    match spec {
        StreamSpec::Pcm(p) => {
            format!("{} Hz {} ch {}-bit/{:?}", p.rate, p.channels, p.bits_valid, p.format)
        }
        StreamSpec::Dsd(d) => format!("DSD {:?} {} ch", d.rate, d.channels),
    }
}

// --- Probe -----------------------------------------------------------------

/// `--probe`: enumerate devices and print their capabilities.
fn run_probe(cli: &Cli) -> Result<(), String> {
    match &cli.device {
        Some(id) => {
            let probe = lyra_output::probe(&DeviceId::Endpoint(id.clone()))
                .map_err(|e| format!("probe failed: {e}"))?;
            print_probe(&probe);
        }
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
            println!("    {:>7} Hz  {:<4} {}-bit valid", f.rate, format!("{:?}", f.format), f.valid_bits);
        }
    }
    println!("  DoP capable: {}  native DSD rates: {}", p.dop_capable, p.native_dsd_rates.len());
}

// --- Raw-PCM (legacy P2) mode ----------------------------------------------

/// Play one raw-PCM file straight through lyra-output (unchanged from P2).
fn run_raw(cli: &Cli) -> Result<(), String> {
    let input = cli.inputs.first().ok_or("a raw input file is required")?;
    let rate = cli.rate.ok_or("--rate is required to play raw PCM")?;
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
/// thread, and push into the ring with simple back-pressure.
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
    let mut carry = 0usize;
    let mut padded: Vec<u8> = Vec::new();
    let mut frames_pushed: u64 = 0;

    loop {
        if frames_pushed >= max_frames {
            println!("Reached --max-seconds cap ({max_seconds}s); stopping.");
            break;
        }
        let n = reader.read(&mut inbuf[carry..]).map_err(|e| format!("read input: {e}"))?;
        if n == 0 && carry == 0 {
            break;
        }
        let avail = carry + n;
        let whole = (avail / src_frame) * src_frame;
        if whole == 0 {
            if n == 0 {
                break;
            }
            carry = avail;
            continue;
        }
        lyra_output::pad_frames(source, negotiated.format, &inbuf[..whole], &mut padded);
        push_all(handle, &padded)?;
        frames_pushed += (whole / src_frame) as u64;
        carry = avail - whole;
        inbuf.copy_within(whole..avail, 0);
        if n == 0 {
            break;
        }
    }
    Ok(frames_pushed)
}

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
    thread::sleep(Duration::from_millis(150));
}

fn rate_cap(rate: u32, seconds: u64) -> u64 {
    u64::from(rate).saturating_mul(seconds)
}
