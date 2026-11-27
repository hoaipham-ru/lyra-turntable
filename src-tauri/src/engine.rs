//! Native lyra-engine control surface for the webview (P4.1).
//!
//! The React `NativeBackend` (P4.3, a separate task) drives the native engine
//! entirely through the commands here plus one event stream:
//!
//! * [`engine_start`] / [`engine_send`] / [`engine_shutdown`] — lifecycle and
//!   transport. One [`Engine`] is created lazily and kept in Tauri state; sending
//!   a command before an explicit start lazily starts it from persisted settings.
//! * [`devices`] — enumerate render endpoints with their exclusive-mode caps.
//! * Event stream — every [`EngineEvent`] is forwarded to the webview on the
//!   `engine://event` channel as a serde-tagged [`EnginePayload`] (see its docs
//!   for the exact JSON shape, including the pre-built format badge string).
//!
//! # Units
//! Positions and durations travel to the webview as both source **frames** and
//! derived **seconds**. `Seek` is accepted from the webview in **seconds** and
//! converted to a source frame here using the last sample rate seen on a `Track`
//! or `Format` event (so the webview never needs the source spec to seek).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use lyra_core::{DsdRate, EngineCommand, EngineEvent, PlayState, SampleFormat, StreamSpec, TrackInfo, TrackRef};
use lyra_engine::Engine;
use lyra_output::{DeviceId, ExclusiveFormat};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::settings;

/// The event channel the webview listens on for engine events.
pub const EVENT_CHANNEL: &str = "engine://event";

/// Shared, cheaply-cloneable context the event thread and the command handlers
/// both touch: the last known source sample rate (for seconds↔frames) and the
/// current track's container extension (for the format badge).
#[derive(Default)]
struct Shared {
    source_rate: AtomicU32,
    track_ext: Mutex<Option<String>>,
}

/// One running engine plus its event-forwarding thread.
struct EngineRuntime {
    /// `None` only transiently while shutting down (the handle is consumed by
    /// [`EngineHandle::shutdown`]).
    handle: Option<lyra_engine::EngineHandle>,
    event_thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

/// Tauri-managed state: at most one engine at a time.
#[derive(Default)]
pub struct EngineState(Mutex<Option<EngineRuntime>>);

// --- Transport commands from the webview -----------------------------------

/// A transport command from the webview, mirroring [`EngineCommand`].
///
/// Serde-tagged by `type` (camelCase), e.g. `{ "type": "playAt", "index": 2 }`
/// or `{ "type": "seek", "seconds": 42.5 }`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UiCommand {
    /// Start or resume playback.
    Play,
    /// Pause, keeping position.
    Pause,
    /// Stop and reset position.
    Stop,
    /// Advance to the next queued track.
    Next,
    /// Go back to the previous queued track.
    Prev,
    /// Play the queue entry at this zero-based index.
    PlayAt {
        /// Zero-based queue index.
        index: usize,
    },
    /// Seek to a position in **seconds** (converted to a source frame here).
    Seek {
        /// Target position in seconds from the start of the track.
        seconds: f64,
    },
    /// Replace the play queue with these file paths (each becomes a whole-file
    /// [`TrackRef`]).
    SetQueue {
        /// Source file paths, in queue order.
        paths: Vec<String>,
    },
}

// --- Event payload to the webview ------------------------------------------

/// A single engine event, serialized for the webview on [`EVENT_CHANNEL`].
///
/// Tagged by `kind` (camelCase). The variants map 1:1 to [`EngineEvent`] but add
/// webview-friendly derived fields: seconds alongside frames, and a ready-to-show
/// format badge string so the UI never has to assemble it.
#[derive(Debug, Clone, Serialize)]
// `rename_all` only renames the variant tags; `rename_all_fields` is what turns the
// struct-variant fields (duration_seconds, bit_perfect, source_desc, ...) into the
// camelCase names the webview reads. Without it the track length was never shown
// and seeking stayed disabled (2026-11-27).
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EnginePayload {
    /// Transport state changed. `state` is `"playing" | "paused" | "stopped"`.
    State {
        /// Lowercase state name.
        state: &'static str,
    },
    /// Playback position advanced.
    Position {
        /// Source frame.
        frame: u64,
        /// `frame` converted to seconds using the last known source rate.
        seconds: f64,
    },
    /// A new track started.
    Track {
        /// Source file path.
        path: String,
        /// Title, if tagged.
        title: Option<String>,
        /// Artist, if tagged.
        artist: Option<String>,
        /// Album, if tagged.
        album: Option<String>,
        /// Exact length in source frames, if known.
        duration_frames: Option<u64>,
        /// `duration_frames` converted to seconds, if known.
        duration_seconds: Option<f64>,
        /// Upper-cased container extension, e.g. `"FLAC"` (best-effort, from the
        /// path).
        source: Option<String>,
    },
    /// The source/device formats were negotiated.
    Format {
        /// Short source format, e.g. `"24/96"` or `"DSD64"`.
        source_desc: String,
        /// Short device format, e.g. `"24/96"`.
        device_desc: String,
        /// `true` when the device stream equals the source bit-for-bit (ADR-004).
        bit_perfect: bool,
        /// Ready-to-display badge, e.g. `"FLAC 24/96 → 24/96 ✓"`. Built from the
        /// current track's container extension, the source and device formats and
        /// the bit-perfect flag; `✓` when bit-perfect, `✗` otherwise.
        badge: String,
    },
    /// The output ring buffer ran dry; `count` is the running total.
    Underrun {
        /// Cumulative underrun count.
        count: u64,
    },
    /// A recoverable or fatal error occurred.
    Error {
        /// Human-readable message.
        message: String,
    },
}

// --- Formatting helpers (shared with the badge builder) --------------------

/// Sample rate in kHz, trimmed: `96000 → "96"`, `44100 → "44.1"`.
fn khz(rate: u32) -> String {
    let k = f64::from(rate) / 1000.0;
    if (k.fract()).abs() < 1e-6 {
        format!("{}", k as u64)
    } else {
        format!("{k:.1}")
    }
}

/// Human DSD label, e.g. `"DSD64"`.
fn dsd_label(rate: DsdRate) -> &'static str {
    match rate {
        DsdRate::Dsd64 => "DSD64",
        DsdRate::Dsd128 => "DSD128",
        DsdRate::Dsd256 => "DSD256",
        DsdRate::Dsd512 => "DSD512",
    }
}

/// Short format description: `"24/96"` for PCM, `"DSD64"` for DSD.
fn spec_desc(spec: &StreamSpec) -> String {
    match spec {
        StreamSpec::Pcm(p) => format!("{}/{}", p.bits_valid, khz(p.rate)),
        StreamSpec::Dsd(d) => dsd_label(d.rate).to_string(),
    }
}

/// The sample rate used for seconds↔frames: the PCM rate, or the raw DSD rate.
fn spec_rate(spec: &StreamSpec) -> u32 {
    match spec {
        StreamSpec::Pcm(p) => p.rate,
        StreamSpec::Dsd(d) => d.rate.hz(),
    }
}

/// Short container name from a path extension, upper-cased (`"FLAC"`).
fn ext_codec(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_uppercase)
}

/// `SampleFormat` as a short string for the device-caps payload.
fn format_name(f: SampleFormat) -> &'static str {
    match f {
        SampleFormat::S16 => "S16",
        SampleFormat::S24 => "S24",
        SampleFormat::S32 => "S32",
        SampleFormat::F32 => "F32",
    }
}

/// Convert a frame count to seconds given a sample rate (0 rate → 0.0).
fn frames_to_seconds(frame: u64, rate: u32) -> f64 {
    if rate == 0 {
        0.0
    } else {
        frame as f64 / f64::from(rate)
    }
}

/// Turn one [`EngineEvent`] into its webview payload, updating `shared` with the
/// rate/extension the later `Position`/`Seek` math depends on.
fn to_payload(ev: EngineEvent, shared: &Shared) -> EnginePayload {
    match ev {
        EngineEvent::State(s) => EnginePayload::State {
            state: match s {
                PlayState::Playing => "playing",
                PlayState::Paused => "paused",
                PlayState::Stopped => "stopped",
            },
        },
        EngineEvent::Position { frame } => {
            let rate = shared.source_rate.load(Ordering::Relaxed);
            EnginePayload::Position { frame, seconds: frames_to_seconds(frame, rate) }
        }
        EngineEvent::Track(TrackInfo { track, title, artist, album, duration_frames, spec }) => {
            let rate = spec_rate(&spec);
            shared.source_rate.store(rate, Ordering::Relaxed);
            let source = ext_codec(&track.path);
            *shared.track_ext.lock().unwrap() = source.clone();
            EnginePayload::Track {
                path: track.path,
                title,
                artist,
                album,
                duration_frames,
                duration_seconds: duration_frames.map(|f| frames_to_seconds(f, rate)),
                source,
            }
        }
        EngineEvent::Format { source, device, bit_perfect } => {
            shared.source_rate.store(spec_rate(&source), Ordering::Relaxed);
            let source_desc = spec_desc(&source);
            let device_desc = spec_desc(&device);
            let mark = if bit_perfect { "✓" } else { "✗" };
            let codec = shared.track_ext.lock().unwrap().clone();
            let badge = match codec {
                Some(c) => format!("{c} {source_desc} → {device_desc} {mark}"),
                None => format!("{source_desc} → {device_desc} {mark}"),
            };
            EnginePayload::Format { source_desc, device_desc, bit_perfect, badge }
        }
        EngineEvent::Underrun { count } => EnginePayload::Underrun { count },
        EngineEvent::Error(e) => EnginePayload::Error { message: e.to_string() },
    }
}

// --- Lifecycle --------------------------------------------------------------

/// Start (or restart) the engine with these output options and begin forwarding
/// its events to the webview. Restarting first tears down any running engine, so
/// two engines never produce sound at once.
fn start(app: &AppHandle, state: &EngineState, config: lyra_engine::EngineConfig) -> Result<(), String> {
    // Tear down an existing engine before opening another device.
    stop(state);

    let (handle, rx) = Engine::new(config).map_err(|e| e.to_string())?;
    let shared = Arc::new(Shared::default());
    let thread_shared = Arc::clone(&shared);
    let app = app.clone();
    let event_thread = std::thread::Builder::new()
        .name("lyra-engine-events".into())
        .spawn(move || {
            // `rx.iter()` ends when the engine drops its event sender during
            // shutdown, so this thread exits on its own and joins cleanly.
            for ev in rx.iter() {
                let payload = to_payload(ev, &thread_shared);
                let _ = app.emit(EVENT_CHANNEL, payload);
            }
        })
        .map_err(|e| e.to_string())?;

    *state.0.lock().unwrap() = Some(EngineRuntime { handle: Some(handle), event_thread: Some(event_thread), shared });
    Ok(())
}

/// Shut the running engine down and join its event thread. No-op if not running.
fn stop(state: &EngineState) {
    let runtime = state.0.lock().unwrap().take();
    if let Some(mut runtime) = runtime {
        if let Some(handle) = runtime.handle.take() {
            handle.shutdown(); // joins the engine's own threads, releases the device
        }
        if let Some(join) = runtime.event_thread.take() {
            join.join().ok();
        }
    }
}

/// Ensure an engine is running, lazily starting it from persisted settings.
fn ensure_started(app: &AppHandle, state: &EngineState) -> Result<(), String> {
    if state.0.lock().unwrap().is_some() {
        return Ok(());
    }
    start(app, state, settings::load(app).to_engine_config())
}

// --- Commands ---------------------------------------------------------------

/// Output options the webview passes to [`engine_start`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineConfigDto {
    /// Endpoint id, or empty for the system default.
    pub device_id: String,
    /// `"exclusive" | "shared" | "asio"`.
    pub mode: String,
    /// `"auto" | "dop" | "native" | "pcm"`.
    pub dsd_mode: String,
}

/// Start (or restart) the engine with explicit output options. Non-blocking
/// relative to decode/device I/O (the engine opens its device only on first play).
#[tauri::command]
pub fn engine_start(app: AppHandle, state: State<'_, EngineState>, config: EngineConfigDto) -> Result<(), String> {
    let cfg = settings::engine_config(&config.device_id, &config.mode, &config.dsd_mode);
    start(&app, &state, cfg)
}

/// Shut the native engine down and release its device. Call when switching away
/// from the native backend and on app exit.
#[tauri::command]
pub fn engine_shutdown(state: State<'_, EngineState>) {
    stop(&state);
}

/// `true` if the native engine is currently running.
#[tauri::command]
pub fn engine_running(state: State<'_, EngineState>) -> bool {
    state.0.lock().unwrap().is_some()
}

/// Send a transport command, lazily starting the engine from persisted settings
/// on the first command. Never blocks on decode or device I/O.
#[tauri::command]
pub fn engine_send(app: AppHandle, state: State<'_, EngineState>, cmd: UiCommand) -> Result<(), String> {
    ensure_started(&app, &state)?;
    let guard = state.0.lock().unwrap();
    let runtime = guard.as_ref().ok_or("engine not running")?;
    let engine_cmd = match cmd {
        UiCommand::Play => EngineCommand::Play,
        UiCommand::Pause => EngineCommand::Pause,
        UiCommand::Stop => EngineCommand::Stop,
        UiCommand::Next => EngineCommand::Next,
        UiCommand::Prev => EngineCommand::Prev,
        UiCommand::PlayAt { index } => EngineCommand::PlayAt(index),
        UiCommand::Seek { seconds } => {
            let rate = runtime.shared.source_rate.load(Ordering::Relaxed);
            let frame = (seconds.max(0.0) * f64::from(rate)).round() as u64;
            EngineCommand::Seek(frame)
        }
        UiCommand::SetQueue { paths } => {
            EngineCommand::SetQueue(paths.into_iter().map(TrackRef::file).collect())
        }
    };
    if let Some(handle) = runtime.handle.as_ref() {
        handle.send(engine_cmd);
    }
    Ok(())
}

// --- Device enumeration -----------------------------------------------------

/// One probed exclusive-mode format, for the device-caps payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatDto {
    /// Sample rate in Hz.
    pub rate: u32,
    /// Container word, `"S16" | "S24" | "S32" | "F32"`.
    pub format: &'static str,
    /// Valid bits advertised to the driver.
    pub valid_bits: u8,
}

impl From<&ExclusiveFormat> for FormatDto {
    fn from(f: &ExclusiveFormat) -> Self {
        Self { rate: f.rate, format: format_name(f.format), valid_bits: f.valid_bits }
    }
}

/// A render endpoint with its exclusive-mode capabilities, for the settings UI.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    /// Stable WASAPI endpoint id (empty-safe: pass back as `device_id`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// `true` if this is the current default render endpoint.
    pub is_default: bool,
    /// Exclusive-mode formats the device accepted on the probe grid. Empty when
    /// probing failed for this device.
    pub exclusive_formats: Vec<FormatDto>,
    /// Marked DoP-capable (never assumed for unknown devices, ADR-009).
    pub dop_capable: bool,
    /// Native DSD rates over ASIO, e.g. `["DSD64", "DSD128"]` (empty without an
    /// ASIO driver).
    pub native_dsd_rates: Vec<&'static str>,
}

/// Enumerate render endpoints and probe each for its exclusive-mode caps.
///
/// Probing calls `IsFormatSupported` (no stream is opened for playback); a device
/// that fails to probe is still returned with empty `exclusive_formats`.
#[tauri::command]
pub fn devices() -> Result<Vec<DeviceDto>, String> {
    let infos = lyra_output::devices().map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(infos.len());
    for info in infos {
        let probe = lyra_output::probe(&DeviceId::Endpoint(info.id.clone())).ok();
        let (exclusive_formats, dop_capable, native_dsd_rates) = match probe {
            Some(p) => (
                p.exclusive_formats.iter().map(FormatDto::from).collect(),
                p.dop_capable,
                p.native_dsd_rates.iter().map(|r| dsd_label(*r)).collect(),
            ),
            None => (Vec::new(), false, Vec::new()),
        };
        out.push(DeviceDto {
            id: info.id,
            name: info.name,
            is_default: info.is_default,
            exclusive_formats,
            dop_capable,
            native_dsd_rates,
        });
    }
    Ok(out)
}

/// Shut the engine down from the app-exit hook (takes an [`AppHandle`] so it can
/// reach the managed state without a command context).
pub fn shutdown_on_exit(app: &AppHandle) {
    if let Some(state) = app.try_state::<EngineState>() {
        stop(&state);
    }
}

#[cfg(test)]
mod payload_field_names {
    use super::EnginePayload;

    // The webview reads camelCase field names (durationSeconds, bitPerfect, ...).
    // Regression test for the 1.0.0 bug where they went out as snake_case, so the
    // track length showed 0:00 and the seek bar stayed disabled.
    #[test]
    fn struct_variant_fields_are_camel_case() {
        let track = EnginePayload::Track {
            path: "a.dff".into(),
            title: None,
            artist: None,
            album: None,
            duration_frames: Some(994_049_280),
            duration_seconds: Some(352.2),
            source: Some("DFF".into()),
        };
        let j = serde_json::to_value(&track).unwrap();
        assert_eq!(j["kind"], "track");
        assert_eq!(j["durationSeconds"], 352.2);
        assert_eq!(j["durationFrames"], 994_049_280u64);
        assert!(j.get("duration_seconds").is_none());

        let fmt = EnginePayload::Format { source_desc: "DSD64".into(), device_desc: "24/176.4".into(), bit_perfect: false, badge: "b".into() };
        let j = serde_json::to_value(&fmt).unwrap();
        assert_eq!(j["bitPerfect"], false);
        assert_eq!(j["sourceDesc"], "DSD64");
    }
}