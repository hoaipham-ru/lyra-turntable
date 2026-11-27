//! The WASAPI backend (Windows): device enumeration, `IsFormatSupported`
//! capability probing, the event-driven exclusive render stream with a push-mode
//! fallback, shared mode (always flagged *not* bit-perfect), device-lost handling
//! and the exclusive capture helper.
//!
//! # Threading and COM
//! Every COM object is created and used on the thread that initialised COM in the
//! multithreaded apartment. [`devices`], [`probe`] and [`capture_exclusive`] run
//! fully on the caller's thread inside one [`ComGuard`]. [`open`] spawns a
//! dedicated **feeder thread** that initialises its own MTA, opens the device,
//! negotiates the format, and runs the render loop; nothing COM ever crosses a
//! thread boundary. The feeder sends the owner side only plain `Send` values
//! (the ring producer, the negotiated spec, the tap drain).
//!
//! # Real-time rules on the feeder loop
//! Once running, the loop only: waits the event handle, asks the device how much
//! space is free, pulls that many bytes from the ring with a lock-free, no-alloc
//! copy ([`crate::ring::pull_into`]), copies them into the device buffer via the
//! `wasapi` API, optionally copies them into the tap ring, and bumps relaxed
//! atomics. No allocation, no locks, no logging, no file I/O (PLAN §5).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use lyra_core::{LyraError, OutputMode, PcmSpec, Result, SampleFormat, StreamSpec};
use wasapi::{
    deinitialize, get_default_device, initialize_mta, AudioCaptureClient, AudioClient,
    AudioRenderClient, Device, DeviceCollection, Direction, Handle, SampleType, ShareMode,
    WaveFormat,
};
use windows::Win32::Media::Audio::AUDCLNT_E_DEVICE_INVALIDATED;

use crate::handle::OutputHandle;
use crate::mmcss::ProAudioGuard;
use crate::negotiate::{negotiate_exclusive, ExclusiveFormat, PROBE_CANDIDATES};
use crate::ring::{pull_into, service_flush, silence, FlushGrace};
use crate::stats::SharedStats;
use crate::tap::{make_tap, TapDrain, TapWriter};
use crate::types::{DeviceId, DeviceInfo, DeviceProbe, OutputOptions, PROBE_RATES};

// --- small error helpers ---------------------------------------------------

/// Map a `wasapi` error into the shared error type.
fn werr(e: Box<dyn std::error::Error>) -> LyraError {
    LyraError::Device(e.to_string())
}

/// `true` if a `wasapi` error is `AUDCLNT_E_DEVICE_INVALIDATED` (the endpoint was
/// removed, its format changed, or the service stopped — hot-unplug).
fn is_device_lost(e: &(dyn std::error::Error + 'static)) -> bool {
    e.downcast_ref::<windows::core::Error>()
        .is_some_and(|we| we.code() == AUDCLNT_E_DEVICE_INVALIDATED)
}

// --- COM lifetime ----------------------------------------------------------

/// RAII COM initialisation (MTA) for one thread. Calls `CoUninitialize` on drop
/// only if this guard performed the matching `CoInitializeEx`.
struct ComGuard {
    owned: bool,
}

impl ComGuard {
    fn enter() -> Result<Self> {
        let hr = initialize_mta();
        if hr.is_ok() {
            Ok(Self { owned: true })
        } else if hr.0 == 0x8001_0106u32 as i32 {
            // RPC_E_CHANGED_MODE: COM is already initialised on this thread in a
            // different apartment (e.g. STA under Tauri). Use it; do not uninit.
            Ok(Self { owned: false })
        } else {
            Err(LyraError::Device(format!("COM init failed: {hr:?}")))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            deinitialize();
        }
    }
}

// --- format helpers --------------------------------------------------------

/// Build a `WAVEFORMATEXTENSIBLE` (always integer PCM) for the given parameters.
fn wave_format_for(rate: u32, channels: u16, format: SampleFormat, valid_bits: u8) -> WaveFormat {
    let store_bits = format.bytes_per_sample() * 8;
    WaveFormat::new(
        store_bits,
        valid_bits as usize,
        &SampleType::Int,
        rate as usize,
        channels as usize,
        None,
    )
}

/// Ask the driver (via `IsFormatSupported`) whether one exclusive candidate is
/// accepted at `rate`. Retries 1- and 2-channel formats as a plain `WAVEFORMATEX`,
/// which some drivers accept when they reject the extensible form (wasapi docs).
fn is_exclusive_supported(
    client: &AudioClient,
    rate: u32,
    channels: u16,
    format: SampleFormat,
    valid_bits: u8,
) -> bool {
    let wf = wave_format_for(rate, channels, format, valid_bits);
    if client.is_supported(&wf, &ShareMode::Exclusive).is_ok() {
        return true;
    }
    if channels <= 2 {
        if let Ok(wfex) = wf.to_waveformatex() {
            if client.is_supported(&wfex, &ShareMode::Exclusive).is_ok() {
                return true;
            }
        }
    }
    false
}

/// Every exclusive container the device accepts for `source.rate`.
fn supported_exclusive(client: &AudioClient, source: PcmSpec) -> Vec<ExclusiveFormat> {
    let mut out = Vec::new();
    for &(format, valid_bits) in &PROBE_CANDIDATES {
        if is_exclusive_supported(client, source.rate, source.channels, format, valid_bits) {
            out.push(ExclusiveFormat { rate: source.rate, format, valid_bits });
        }
    }
    out
}

// --- endpoint lookup -------------------------------------------------------

fn default_id(direction: &Direction) -> Option<String> {
    get_default_device(direction).ok().and_then(|d| d.get_id().ok())
}

/// Open the requested render/capture endpoint (or the default).
fn open_endpoint(dev: &DeviceId, direction: Direction) -> Result<Device> {
    match dev.as_str() {
        None => get_default_device(&direction).map_err(werr),
        Some(id) => {
            let coll = DeviceCollection::new(&direction).map_err(werr)?;
            let n = coll.get_nbr_devices().map_err(werr)?;
            for i in 0..n {
                let d = coll.get_device_at_index(i).map_err(werr)?;
                if d.get_id().map_err(werr)? == id {
                    return Ok(d);
                }
            }
            Err(LyraError::Device(format!("no {direction} endpoint with id {id}")))
        }
    }
}

// --- public: enumeration + probe ------------------------------------------

/// Enumerate the active render endpoints.
///
/// # Errors
/// Fails if COM or the device enumerator cannot be initialised.
pub fn devices() -> Result<Vec<DeviceInfo>> {
    let _com = ComGuard::enter()?;
    let coll = DeviceCollection::new(&Direction::Render).map_err(werr)?;
    let n = coll.get_nbr_devices().map_err(werr)?;
    let def = default_id(&Direction::Render);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let d = coll.get_device_at_index(i).map_err(werr)?;
        let id = d.get_id().map_err(werr)?;
        let name = d.get_friendlyname().unwrap_or_else(|_| "Unknown device".into());
        let is_default = def.as_deref() == Some(id.as_str());
        out.push(DeviceInfo { id, name, is_default });
    }
    Ok(out)
}

/// Probe one endpoint's exclusive capabilities (rates × containers) and its
/// shared mix rate (`lyra-play --probe`, ADR-009). DoP and native DSD are left
/// off here — a built-in codec supports neither, and ASIO/DoP land in P5.
///
/// # Errors
/// Fails if the endpoint or its audio client cannot be opened.
pub fn probe(dev: &DeviceId) -> Result<DeviceProbe> {
    let _com = ComGuard::enter()?;
    let device = open_endpoint(dev, Direction::Render)?;
    let id = device.get_id().map_err(werr)?;
    let name = device.get_friendlyname().unwrap_or_else(|_| "Unknown device".into());
    let is_default = default_id(&Direction::Render).as_deref() == Some(id.as_str());
    let info = DeviceInfo { id, name, is_default };

    let client = device.get_iaudioclient().map_err(werr)?;
    let shared_mix_rate = client.get_mixformat().ok().map(|f| f.get_samplespersec());

    let mut exclusive_formats = Vec::new();
    for &rate in &PROBE_RATES {
        for &(format, valid_bits) in &PROBE_CANDIDATES {
            if is_exclusive_supported(&client, rate, 2, format, valid_bits) {
                exclusive_formats.push(ExclusiveFormat { rate, format, valid_bits });
            }
        }
    }

    Ok(DeviceProbe {
        info,
        shared_mix_rate,
        exclusive_formats,
        dop_capable: false,
        native_dsd_rates: Vec::new(),
    })
}

// --- public: open ----------------------------------------------------------

/// Open `dev` for `spec` in `mode` and start the feeder thread.
///
/// On success the returned [`OutputHandle`] carries the ring producer (push
/// device-format bytes), the negotiated format, the live counters and, if a tap
/// was requested, the running tap-drain thread. The call blocks only until the
/// feeder has opened and started the device (or reported why it could not).
///
/// # Errors
/// - [`LyraError::UnsupportedFormat`] for a DSD spec (DoP/native DSD is P5) or an
///   ASIO mode (P5).
/// - [`LyraError::Device`] if the device rejects the source rate/format without
///   resampling, or any device call fails.
pub fn open(
    dev: &DeviceId,
    spec: &StreamSpec,
    mode: OutputMode,
    opts: OutputOptions,
) -> Result<OutputHandle> {
    let source = match spec {
        StreamSpec::Pcm(p) => *p,
        StreamSpec::Dsd(_) => {
            return Err(LyraError::UnsupportedFormat(
                "DSD output (DoP / native) is a P5 feature".into(),
            ))
        }
    };
    if let OutputMode::Asio { .. } = mode {
        return Err(LyraError::UnsupportedFormat("ASIO output is a P5 feature".into()));
    }

    let stats = Arc::new(SharedStats::default());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cfg = FeederConfig {
        dev: dev.clone(),
        source,
        mode,
        tap_path: opts.tap_path.clone(),
        allow_poll_fallback: opts.allow_poll_fallback,
    };

    let (ready_tx, ready_rx) = mpsc::channel::<Result<Ready>>();
    let stats_f = Arc::clone(&stats);
    let stop_f = Arc::clone(&stop);
    let feeder = thread::Builder::new()
        .name("lyra-output-feeder".into())
        .spawn(move || feeder_main(cfg, stats_f, stop_f, ready_tx))
        .map_err(|e| LyraError::Device(format!("spawn feeder thread: {e}")))?;

    let ready = match ready_rx.recv() {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            let _ = feeder.join();
            return Err(e);
        }
        Err(_) => {
            let _ = feeder.join();
            return Err(LyraError::Device("feeder thread exited before init".into()));
        }
    };

    let (tap_drain, tap_dropped) = match ready.tap {
        Some((drain, dropped)) => {
            let h = thread::Builder::new()
                .name("lyra-output-tap".into())
                .spawn(move || drain.run())
                .map_err(|e| LyraError::Device(format!("spawn tap thread: {e}")))?;
            (Some(h), Some(dropped))
        }
        None => (None, None),
    };

    Ok(OutputHandle {
        producer: ready.producer,
        negotiated: ready.negotiated,
        bit_perfect: ready.bit_perfect,
        stats,
        stop,
        feeder: Some(feeder),
        tap_drain,
        tap_dropped,
    })
}

// --- feeder internals ------------------------------------------------------

struct FeederConfig {
    dev: DeviceId,
    source: PcmSpec,
    mode: OutputMode,
    tap_path: Option<PathBuf>,
    allow_poll_fallback: bool,
}

/// The `Send` values handed back to [`open`] once the stream is live.
struct Ready {
    negotiated: StreamSpec,
    producer: rtrb::Producer<u8>,
    bit_perfect: bool,
    tap: Option<(TapDrain, Arc<AtomicU64>)>,
}

/// Everything the render loop owns (all on the feeder thread; none `Send`).
struct RunState {
    client: AudioClient,
    render: AudioRenderClient,
    event: Option<Handle>,
    consumer: rtrb::Consumer<u8>,
    scratch: Vec<u8>,
    buffer_frames: usize,
    bytes_per_frame: usize,
    tap: Option<TapWriter>,
    period_ms: u32,
}

fn feeder_main(
    cfg: FeederConfig,
    stats: Arc<SharedStats>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    ready_tx: mpsc::Sender<Result<Ready>>,
) {
    let _com = match ComGuard::enter() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    match setup_stream(&cfg) {
        Ok((mut run, ready)) => {
            // If the owner already hung up, do not start the device.
            if ready_tx.send(Ok(ready)).is_ok() {
                run_loop(&mut run, &stats, &stop);
            }
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
        }
    }
}

fn setup_stream(cfg: &FeederConfig) -> Result<(RunState, Ready)> {
    let device = open_endpoint(&cfg.dev, Direction::Render)?;
    let mut client = device.get_iaudioclient().map_err(werr)?;

    // Decide format + container and whether the path is bit-perfect.
    let (wave_fmt, negotiated_spec, device_spec, bit_perfect, convert) = match cfg.mode {
        OutputMode::WasapiExclusive => {
            let supported = supported_exclusive(&client, cfg.source);
            let neg = negotiate_exclusive(cfg.source, &supported)?;
            let wf = wave_format_for(
                cfg.source.rate,
                cfg.source.channels,
                neg.spec.format,
                neg.request_valid_bits,
            );
            (wf, StreamSpec::Pcm(neg.spec), neg.spec, true, false)
        }
        OutputMode::WasapiShared => {
            // Shared mode is fed in the *source* format; Windows mixes/converts it,
            // so it is never bit-perfect on this path (ADR-004).
            let wf = wave_format_for(
                cfg.source.rate,
                cfg.source.channels,
                cfg.source.format,
                cfg.source.bits_valid,
            );
            (wf, StreamSpec::Pcm(cfg.source), cfg.source, false, true)
        }
        OutputMode::Asio { .. } => {
            return Err(LyraError::UnsupportedFormat("ASIO output is a P5 feature".into()))
        }
    };

    let share = match cfg.mode {
        OutputMode::WasapiShared => ShareMode::Shared,
        _ => ShareMode::Exclusive,
    };

    let (def_period, _min_period) = client.get_periods().map_err(werr)?;
    let period = match share {
        ShareMode::Exclusive => client
            .calculate_aligned_period_near(def_period, Some(128), &wave_fmt)
            .map_err(werr)?,
        ShareMode::Shared => def_period,
    };

    client
        .initialize_client(&wave_fmt, period, &Direction::Render, &share, convert)
        .map_err(werr)?;

    // Event-driven by default; fall back to timer/push mode only if asked.
    let event = match client.set_get_eventhandle() {
        Ok(h) => Some(h),
        Err(e) if cfg.allow_poll_fallback => {
            // Push mode: no event handle, the loop uses a timer. (Allowed only
            // when the caller opted in.)
            let _ = e;
            None
        }
        Err(e) => return Err(LyraError::Device(format!("event handle setup failed: {e}"))),
    };

    let buffer_frames = client.get_bufferframecount().map_err(werr)? as usize;
    let render = client.get_audiorenderclient().map_err(werr)?;
    let bytes_per_frame = device_spec.bytes_per_frame();

    // ~2 s decode-side ring (S04), at least a few device buffers.
    let two_sec = bytes_per_frame * device_spec.rate as usize * 2;
    let floor = bytes_per_frame * buffer_frames * 4;
    let (producer, consumer) = rtrb::RingBuffer::<u8>::new(two_sec.max(floor).max(bytes_per_frame));

    let scratch = vec![0u8; buffer_frames * bytes_per_frame];

    let (tap, tap_pair) = match &cfg.tap_path {
        None => (None, None),
        Some(path) => {
            let cap = bytes_per_frame * device_spec.rate as usize * 2;
            let (writer, drain) =
                make_tap(path, cap).map_err(|e| LyraError::Io(format!("open tap file: {e}")))?;
            let dropped = writer.dropped_counter();
            (Some(writer), Some((drain, dropped)))
        }
    };

    let run = RunState {
        client,
        render,
        event,
        consumer,
        scratch,
        buffer_frames,
        bytes_per_frame,
        tap,
        period_ms: period_to_ms(period),
    };
    let ready = Ready { negotiated: negotiated_spec, producer, bit_perfect, tap: tap_pair };
    Ok((run, ready))
}

/// The render loop. Runs under MMCSS "Pro Audio"; see the module docs for the
/// real-time rules it obeys.
fn run_loop(run: &mut RunState, stats: &SharedStats, stop: &std::sync::atomic::AtomicBool) {
    let _mmcss = ProAudioGuard::enter();

    // Pre-fill the whole device buffer with silence so the first event does not
    // underrun (zeros for PCM; a DoP idle pattern will replace this in P5).
    if prefill_silence(run).is_err() {
        stats.device_lost.store(true, Ordering::Relaxed);
        return;
    }
    if run.client.start_stream().is_err() {
        stats.device_lost.store(true, Ordering::Relaxed);
        return;
    }

    // Post-flush reprime grace (ADR-013 extension; see `FlushGrace`). A flush is a
    // deliberate discontinuity, and the engine reprimes the ring to its cushion
    // right after the drain; the still-empty ring during that reprime must not be
    // counted as an underrun. Exit grace once the ring holds a whole device buffer
    // again (`low_water`), or after `grace_cap` feeder iterations so a producer that
    // never reprimes still surfaces underruns.
    let low_water = run.buffer_frames * run.bytes_per_frame;
    // Safety cap ≈ 1 s of feeder iterations (one iteration ≈ one device period).
    // The low-water early-exit ends grace as soon as the reprime lands — normally a
    // few device periods (the soak saw ~3-4 empty buffers per transition) — so the
    // cap only bounds the pathological "producer never reprimes" case: a genuine
    // post-flush stall surfaces within ~1 s instead of being masked forever.
    let grace_cap = (1000 / run.period_ms.max(1)).max(16);
    let mut grace = FlushGrace::new(low_water, grace_cap);

    while !stop.load(Ordering::Acquire) {
        // Wait for the device to want more data.
        match &run.event {
            Some(h) => {
                // 200 ms timeout so we re-check `stop` even if the device is quiet.
                if h.wait_for_event(200).is_err() {
                    continue;
                }
            }
            None => {
                // Push mode timer: half the device period.
                thread::sleep(Duration::from_millis(u64::from(run.period_ms.max(2) / 2)));
            }
        }

        // Discard-on-seek (reviewer B1): if the owner requested a flush, drain the
        // ring to empty here — only this thread owns the consumer. The buffer we
        // render this iteration after an empty ring is deliberate silence, so the
        // underrun it would otherwise flag is suppressed below. Wait-free, no alloc.
        let flushed =
            service_flush(&mut run.consumer, &mut run.scratch, &stats.flush_req, &stats.flush_ack);

        let avail = match run.client.get_available_space_in_frames() {
            Ok(a) => a as usize,
            Err(e) => {
                if is_device_lost(e.as_ref()) {
                    stats.device_lost.store(true, Ordering::Relaxed);
                }
                break;
            }
        };
        if avail == 0 {
            continue;
        }

        let frames = avail.min(run.buffer_frames);
        let bytes = frames * run.bytes_per_frame;
        // Sample the ring level after the flush drain and before the pull: it shows
        // how far the producer has reprimed, which drives the grace exit below.
        let ring_level = run.consumer.slots();
        let pull = pull_into(&mut run.consumer, &mut run.scratch[..bytes]);
        let data = &run.scratch[..bytes];
        if let Some(tap) = run.tap.as_mut() {
            tap.push(data);
        }
        match run.render.write_to_device(frames, data, None) {
            Ok(()) => {
                stats.frames_played.fetch_add(frames as u64, Ordering::Relaxed);
                // Count an underrun only when it is genuine starvation. A flush this
                // iteration is an intentional discard, and the bounded post-flush
                // grace covers the reprime window that follows it; steady playback
                // (no flush) still counts every underrun. See `FlushGrace`.
                if grace.count_underrun(flushed, ring_level, pull.underrun) {
                    stats.underruns.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(e) => {
                if is_device_lost(e.as_ref()) {
                    stats.device_lost.store(true, Ordering::Relaxed);
                }
                break;
            }
        }
    }

    let _ = run.client.stop_stream();
}

/// Fill the device buffer once with silence before `Start`.
fn prefill_silence(run: &mut RunState) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let avail = run.client.get_available_space_in_frames()? as usize;
    let frames = avail.min(run.buffer_frames);
    let bytes = frames * run.bytes_per_frame;
    silence(&mut run.scratch[..bytes]);
    let data = &run.scratch[..bytes];
    if let Some(tap) = run.tap.as_mut() {
        tap.push(data);
    }
    run.render.write_to_device(frames, data, None)
}

fn period_to_ms(period_100ns: i64) -> u32 {
    (period_100ns / 10_000).clamp(1, i64::from(u32::MAX)) as u32
}

// --- public: exclusive capture helper (P2.4, loopback A/B) -----------------

/// Configuration for [`capture_exclusive`].
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Capture endpoint (e.g. a virtual-cable input). `Default` = default capture.
    pub dev: DeviceId,
    /// Exclusive format to request from the capture endpoint.
    pub spec: PcmSpec,
    /// Where raw interleaved frames are written.
    pub out: PathBuf,
    /// Stop after this many frames (bounds unattended runs).
    pub max_frames: u64,
}

/// Open a capture endpoint in WASAPI exclusive mode and write raw frames to a
/// file (used with a virtual cable for the loopback A/B test, TEST-STRATEGY §5).
///
/// This blocks the calling thread until `max_frames` are captured or the device
/// stops. It is **not** run in unattended tests (the sound rule); it exists for
/// the human loopback gate.
///
/// # Errors
/// Fails if the endpoint rejects the format or any device/file call fails.
pub fn capture_exclusive(cfg: &CaptureConfig) -> Result<u64> {
    let _com = ComGuard::enter()?;
    let device = open_endpoint(&cfg.dev, Direction::Capture)?;
    let mut client = device.get_iaudioclient().map_err(werr)?;

    let wave_fmt =
        wave_format_for(cfg.spec.rate, cfg.spec.channels, cfg.spec.format, cfg.spec.bits_valid);
    let (def_period, _min) = client.get_periods().map_err(werr)?;
    let period =
        client.calculate_aligned_period_near(def_period, Some(128), &wave_fmt).map_err(werr)?;
    client
        .initialize_client(&wave_fmt, period, &Direction::Capture, &ShareMode::Exclusive, false)
        .map_err(werr)?;
    let event = client.set_get_eventhandle().map_err(werr)?;
    let capture: AudioCaptureClient = client.get_audiocaptureclient().map_err(werr)?;

    let bytes_per_frame = cfg.spec.bytes_per_frame();
    let buffer_frames = client.get_bufferframecount().map_err(werr)? as usize;
    let mut buf = vec![0u8; buffer_frames * bytes_per_frame * 2];
    let mut file =
        BufWriter::new(File::create(&cfg.out).map_err(|e| LyraError::Io(e.to_string()))?);

    client.start_stream().map_err(werr)?;
    let mut total: u64 = 0;
    while total < cfg.max_frames {
        if event.wait_for_event(1000).is_err() {
            break;
        }
        let (frames, _flags) = capture.read_from_device(&mut buf).map_err(werr)?;
        if frames > 0 {
            let bytes = frames as usize * bytes_per_frame;
            file.write_all(&buf[..bytes]).map_err(|e| LyraError::Io(e.to_string()))?;
            total += u64::from(frames);
        }
    }
    let _ = client.stop_stream();
    file.flush().map_err(|e| LyraError::Io(e.to_string()))?;
    Ok(total)
}
