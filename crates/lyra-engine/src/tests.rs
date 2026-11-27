//! In-crate transport/queue/clock/gapless tests driven by fake decoders and a fake
//! sink.
//!
//! No real audio is rendered and no device is opened (the sound rule): the engine's
//! transport is exercised against an in-memory [`PatternDecoder`]/[`MockSource`] and
//! [`MockSink`] injected through [`Engine::with_factories`]. The sink consumes
//! instantly, so `frames_played` tracks pushed frames and a finite decoder reaches
//! end-of-track deterministically; an "endless" decoder keeps a track playing for
//! navigation and seek tests. A recording sink captures the exact device bytes so
//! the gapless join can be proven byte-for-byte without hardware.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use lyra_core::{
    Decoder, EngineCommand, EngineEvent, LyraError, PcmSpec, PlayState, Result, SampleFormat,
    StreamSpec, TagMap, TrackInfo, TrackRef,
};

use crate::engine::Engine;
use crate::handle::EngineHandle;
use crate::sink::AudioSink;
use crate::source::{Delivery, Opened, Resolved, Source};

const SPEC: PcmSpec = PcmSpec { rate: 48_000, channels: 2, bits_valid: 16, format: SampleFormat::S16 };
const SPEC_96: PcmSpec =
    PcmSpec { rate: 96_000, channels: 2, bits_valid: 16, format: SampleFormat::S16 };
const TEST_PREROLL: Duration = Duration::from_secs(5);

// --- Fakes -----------------------------------------------------------------

/// A decoder whose every byte of frame `f` equals `f & 0xFF`, so a concatenation of
/// carved ranges can be checked byte-for-byte and a seek is content-deterministic.
struct PatternDecoder {
    spec: StreamSpec,
    total: Option<u64>,
    pos: u64,
    chunk: u64,
    tags: TagMap,
}

impl PatternDecoder {
    fn new(spec: PcmSpec, total: Option<u64>, pos: u64) -> Self {
        let mut tags = TagMap::new();
        tags.insert("TITLE", "Mock Title");
        tags.insert("ARTIST", "Mock Artist");
        Self { spec: StreamSpec::Pcm(spec), total, pos, chunk: 4096, tags }
    }
}

impl Decoder for PatternDecoder {
    fn spec(&self) -> StreamSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        self.total
    }
    fn seek(&mut self, frame: u64) -> Result<u64> {
        if let Some(total) = self.total {
            if frame > total {
                return Err(LyraError::SeekOutOfRange(frame));
            }
        }
        self.pos = frame;
        Ok(frame)
    }
    fn read(&mut self, out: &mut Vec<u8>) -> Result<usize> {
        let n = match self.total {
            Some(total) => {
                if self.pos >= total {
                    return Ok(0);
                }
                self.chunk.min(total - self.pos)
            }
            None => self.chunk,
        };
        let bpf = match self.spec {
            StreamSpec::Pcm(p) => p.bytes_per_frame(),
            StreamSpec::Dsd(_) => 1,
        };
        for f in self.pos..self.pos + n {
            let byte = (f & 0xFF) as u8;
            for _ in 0..bpf {
                out.push(byte);
            }
        }
        self.pos += n;
        Ok(n as usize)
    }
    fn tags(&self) -> &TagMap {
        &self.tags
    }
}

/// The expected pattern bytes for frames `[start, end)` at `spec`.
fn pattern_bytes(spec: PcmSpec, start: u64, end: u64) -> Vec<u8> {
    let bpf = spec.bytes_per_frame();
    let mut out = Vec::with_capacity(((end - start) as usize) * bpf);
    for f in start..end {
        let byte = (f & 0xFF) as u8;
        for _ in 0..bpf {
            out.push(byte);
        }
    }
    out
}

/// How a single [`TrackRef`] resolves in the fake source.
#[derive(Clone)]
struct TrackSpec {
    image_key: String,
    image_total: Option<u64>,
    virtual_start: u64,
    virtual_end: Option<u64>,
    spec: PcmSpec,
}

/// A fake [`Source`] over a table of [`TrackSpec`]s keyed by [`TrackRef`], counting
/// decoder opens so the "reuse one decoder across adjacent virtual tracks" property
/// can be asserted.
struct MockSource {
    table: HashMap<TrackRef, TrackSpec>,
    default_total: Option<u64>,
    opens: Arc<AtomicUsize>,
}

impl MockSource {
    fn spec_for(&self, track: &TrackRef) -> TrackSpec {
        self.table.get(track).cloned().unwrap_or(TrackSpec {
            image_key: track.path.clone(),
            image_total: self.default_total,
            virtual_start: 0,
            virtual_end: self.default_total,
            spec: SPEC,
        })
    }
}

impl Source for MockSource {
    fn resolve(&mut self, track: &TrackRef) -> Result<Resolved> {
        let ts = self.spec_for(track);
        let source_spec = StreamSpec::Pcm(ts.spec);
        let info = TrackInfo {
            track: track.clone(),
            title: Some("Mock Title".into()),
            artist: Some("Mock Artist".into()),
            album: None,
            duration_frames: ts.virtual_end.map(|e| e - ts.virtual_start),
            spec: source_spec,
        };
        Ok(Resolved {
            image_key: ts.image_key,
            virtual_start: ts.virtual_start,
            virtual_end: ts.virtual_end,
            source_spec,
            device_spec: source_spec,
            processed: false,
            info,
        })
    }

    fn open(&mut self, track: &TrackRef, resolved: &Resolved) -> Result<Opened> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        let spec = match resolved.source_spec {
            StreamSpec::Pcm(p) => p,
            StreamSpec::Dsd(_) => SPEC,
        };
        // The decoder spans the whole image (so reuse across virtual tracks reads
        // past the first track's end); it begins at the virtual start.
        let image_total = self.spec_for(track).image_total;
        let decoder = Box::new(PatternDecoder::new(spec, image_total, resolved.virtual_start));
        Ok(Opened { decoder, delivery: Delivery::Pcm, produced_pcm: Some(spec) })
    }
}

/// Depth of the fake device ring, in frames.
const MOCK_RING_FRAMES: u64 = 16_384;

/// A fake device that models a bounded ring drained at the sample rate: `push`
/// applies back-pressure when the ring is full and `frames_played` advances with
/// wall-clock time (never past what was pushed). This keeps a track playing long
/// enough to seek, and exercises the worker's real back-pressure path. Optionally
/// records the exact device bytes for the gapless byte-equality proof.
struct MockSink {
    spec: StreamSpec,
    bpf: usize,
    rate: u64,
    start: Instant,
    pushed: Arc<AtomicU64>,
    recorder: Option<Arc<Mutex<Vec<u8>>>>,
}

impl MockSink {
    fn played_now(&self) -> u64 {
        let by_time = (self.start.elapsed().as_secs_f64() * self.rate as f64) as u64;
        by_time.min(self.pushed.load(Ordering::Relaxed))
    }
}

impl AudioSink for MockSink {
    fn negotiated(&self) -> StreamSpec {
        self.spec
    }
    fn is_bit_perfect(&self) -> bool {
        true
    }
    fn push(&mut self, bytes: &[u8]) -> usize {
        let played = self.played_now();
        let pushed = self.pushed.load(Ordering::Relaxed);
        let in_ring = pushed.saturating_sub(played);
        let space = MOCK_RING_FRAMES.saturating_sub(in_ring);
        let offered = (bytes.len() / self.bpf) as u64;
        let accept = offered.min(space);
        let accept_bytes = accept as usize * self.bpf;
        if let Some(rec) = &self.recorder {
            rec.lock().unwrap().extend_from_slice(&bytes[..accept_bytes]);
        }
        self.pushed.fetch_add(accept, Ordering::Relaxed);
        accept_bytes
    }
    fn flush(&mut self) {
        // Model the ring emptying: collapse `pushed` onto what has already played
        // so nothing stale remains buffered. `frames_played` (wall-clock capped by
        // `pushed`) does not rewind.
        let played = self.played_now();
        self.pushed.store(played, Ordering::Relaxed);
    }
    fn frames_played(&self) -> u64 {
        self.played_now()
    }
    fn underruns(&self) -> u64 {
        0
    }
    fn device_lost(&self) -> bool {
        false
    }
}

/// Capacity of the test-controlled ring, in frames.
const RING_CAP_FRAMES: u64 = 16_384;

/// A device ring under explicit test control: pushed bytes sit buffered until the
/// test [`consume`](RingState::consume)s them (moving them into the played record),
/// and [`flush`](RingState::flush) drops whatever is still buffered *without*
/// advancing `frames_played`. This models the real device/ring boundary that the
/// offline sink cannot, so a seek's stale pre-seek tail is observable — the B1
/// regression.
struct RingState {
    bpf: usize,
    /// Pushed-but-unconsumed device bytes, FIFO (the ring contents).
    buffer: Mutex<Vec<u8>>,
    /// Bytes the device has "consumed" (played), in order.
    played: Mutex<Vec<u8>>,
    /// Frames consumed so far (== `frames_played`).
    played_frames: AtomicU64,
}

impl RingState {
    fn new(bpf: usize) -> Self {
        Self {
            bpf,
            buffer: Mutex::new(Vec::new()),
            played: Mutex::new(Vec::new()),
            played_frames: AtomicU64::new(0),
        }
    }
    fn buffered_frames(&self) -> u64 {
        (self.buffer.lock().unwrap().len() / self.bpf) as u64
    }
    /// Push up to the ring's free space; return bytes accepted (back-pressure).
    fn push(&self, bytes: &[u8]) -> usize {
        let mut buf = self.buffer.lock().unwrap();
        let in_ring = (buf.len() / self.bpf) as u64;
        let space = RING_CAP_FRAMES.saturating_sub(in_ring);
        let offered = (bytes.len() / self.bpf) as u64;
        let accept = offered.min(space) as usize * self.bpf;
        buf.extend_from_slice(&bytes[..accept]);
        accept
    }
    /// Move up to `frames` from the front of the ring into the played record.
    fn consume(&self, frames: u64) {
        let mut buf = self.buffer.lock().unwrap();
        let avail = (buf.len() / self.bpf) as u64;
        let take = frames.min(avail) as usize * self.bpf;
        let drained: Vec<u8> = buf.drain(..take).collect();
        drop(buf);
        self.played.lock().unwrap().extend_from_slice(&drained);
        self.played_frames.fetch_add((take / self.bpf) as u64, Ordering::Relaxed);
    }
    /// Drop the buffered tail; `frames_played` is unchanged (the B1 contract:
    /// discarded bytes were never played, so they were never counted).
    fn flush(&self) {
        self.buffer.lock().unwrap().clear();
    }
    fn played_record(&self) -> Vec<u8> {
        self.played.lock().unwrap().clone()
    }
}

/// An [`AudioSink`] over a shared [`RingState`], counting `flush` calls so a test
/// can assert seek/stop/set-queue flush and pause does not.
struct RingSink {
    spec: StreamSpec,
    state: Arc<RingState>,
    flush_count: Arc<AtomicUsize>,
}

impl AudioSink for RingSink {
    fn negotiated(&self) -> StreamSpec {
        self.spec
    }
    fn is_bit_perfect(&self) -> bool {
        true
    }
    fn push(&mut self, bytes: &[u8]) -> usize {
        self.state.push(bytes)
    }
    fn flush(&mut self) {
        self.flush_count.fetch_add(1, Ordering::Relaxed);
        self.state.flush();
    }
    fn frames_played(&self) -> u64 {
        self.state.played_frames.load(Ordering::Relaxed)
    }
    fn underruns(&self) -> u64 {
        0
    }
    fn device_lost(&self) -> bool {
        false
    }
}

/// Build an engine over a single-track endless [`MockSource`] and a test-controlled
/// [`RingSink`], returning the shared ring state and flush counter.
fn ring_engine() -> (Arc<RingState>, Arc<AtomicUsize>, EngineHandle, Receiver<EngineEvent>) {
    let opens = Arc::new(AtomicUsize::new(0));
    let source: Box<dyn Source> =
        Box::new(MockSource { table: HashMap::new(), default_total: None, opens });

    let state = Arc::new(RingState::new(SPEC.bytes_per_frame()));
    let flush_count = Arc::new(AtomicUsize::new(0));
    let state_c = state.clone();
    let flush_c = flush_count.clone();
    let sink = Box::new(move |spec: &StreamSpec| -> Result<Box<dyn AudioSink>> {
        Ok(Box::new(RingSink { spec: *spec, state: state_c.clone(), flush_count: flush_c.clone() }))
    });

    let (engine, rx) = Engine::with_factories(source, sink, TEST_PREROLL);
    (state, flush_count, engine, rx)
}

/// Spin until `cond` holds or the timeout elapses; returns the final value.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

/// Builder for an engine over fakes, capturing the open counts and recorded bytes.
struct Harness {
    opens: Arc<AtomicUsize>,
    sink_opens: Arc<AtomicUsize>,
    recorder: Arc<Mutex<Vec<u8>>>,
}

impl Harness {
    fn build(
        table: HashMap<TrackRef, TrackSpec>,
        default_total: Option<u64>,
        record: bool,
    ) -> (Self, EngineHandle, Receiver<EngineEvent>) {
        let opens = Arc::new(AtomicUsize::new(0));
        let sink_opens = Arc::new(AtomicUsize::new(0));
        let recorder = Arc::new(Mutex::new(Vec::new()));

        let source: Box<dyn Source> = Box::new(MockSource {
            table,
            default_total,
            opens: opens.clone(),
        });

        let sink_opens_c = sink_opens.clone();
        let recorder_c = recorder.clone();
        let sink = Box::new(move |spec: &StreamSpec| -> Result<Box<dyn AudioSink>> {
            sink_opens_c.fetch_add(1, Ordering::Relaxed);
            let (bpf, rate) = match spec {
                StreamSpec::Pcm(p) => (p.bytes_per_frame(), u64::from(p.rate)),
                StreamSpec::Dsd(_) => (1, 1),
            };
            Ok(Box::new(MockSink {
                spec: *spec,
                bpf,
                rate,
                start: Instant::now(),
                pushed: Arc::new(AtomicU64::new(0)),
                recorder: if record { Some(recorder_c.clone()) } else { None },
            }))
        });

        let (engine, rx) = Engine::with_factories(source, sink, TEST_PREROLL);
        (Self { opens, sink_opens, recorder }, engine, rx)
    }
}

/// Build an engine whose every track decodes `total` frames (`None` = endless).
fn fake_engine(total: Option<u64>) -> (EngineHandle, Receiver<EngineEvent>) {
    let (_h, engine, rx) = Harness::build(HashMap::new(), total, false);
    (engine, rx)
}

fn tref(path: &str, sub: Option<u32>) -> TrackRef {
    TrackRef { path: path.into(), subtrack: sub }
}

fn queue(paths: &[&str]) -> EngineCommand {
    EngineCommand::SetQueue(paths.iter().map(|p| TrackRef::file(*p)).collect())
}

/// Drain events until `stop` returns true or the timeout elapses.
fn collect_until(
    rx: &Receiver<EngineEvent>,
    timeout: Duration,
    stop: impl Fn(&EngineEvent) -> bool,
) -> Vec<EngineEvent> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout((deadline - now).min(Duration::from_millis(100))) {
            Ok(ev) => {
                let done = stop(&ev);
                out.push(ev);
                if done {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    out
}

/// Events with the position reports filtered out (they vary with timing).
fn without_positions(events: &[EngineEvent]) -> Vec<&EngineEvent> {
    events.iter().filter(|e| !matches!(e, EngineEvent::Position { .. })).collect()
}

fn states(events: &[EngineEvent]) -> Vec<PlayState> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::State(s) => Some(*s),
            _ => None,
        })
        .collect()
}

fn track_paths(events: &[EngineEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Track(info) => Some(info.track.path.clone()),
            _ => None,
        })
        .collect()
}

// --- Transport tests -------------------------------------------------------

#[test]
fn play_single_track_runs_to_stopped() {
    let (engine, rx) = fake_engine(Some(4096));
    engine.send(queue(&["a.flac"]));
    engine.send(EngineCommand::Play);

    let events = collect_until(&rx, Duration::from_secs(2), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });

    let seq = without_positions(&events);
    assert!(matches!(seq[0], EngineEvent::Track(_)), "first: {seq:?}");
    assert!(matches!(seq[1], EngineEvent::Format { .. }), "second: {seq:?}");
    assert!(
        matches!(seq[2], EngineEvent::State(PlayState::Playing)),
        "third: {seq:?}"
    );
    assert_eq!(states(&events), vec![PlayState::Playing, PlayState::Stopped]);
    assert_eq!(track_paths(&events), vec!["a.flac"]);
    assert!(
        events.iter().any(|e| matches!(e, EngineEvent::Position { frame } if *frame == 4096)),
        "expected a final position of 4096: {events:?}"
    );

    engine.shutdown();
}

#[test]
fn format_event_reports_bit_perfect_pcm() {
    let (engine, rx) = fake_engine(None);
    engine.send(queue(&["a.flac"]));
    engine.send(EngineCommand::Play);

    let events = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::Format { .. })
    });
    let format = events.iter().find_map(|e| match e {
        EngineEvent::Format { source, device, bit_perfect } => Some((*source, *device, *bit_perfect)),
        _ => None,
    });
    let (source, device, bit_perfect) = format.expect("a Format event");
    assert_eq!(source, StreamSpec::Pcm(SPEC));
    assert_eq!(device, StreamSpec::Pcm(SPEC));
    assert!(bit_perfect, "exclusive PCM with no processing is bit-perfect");

    engine.shutdown();
}

#[test]
fn pause_keeps_loaded_then_resumes() {
    let (engine, rx) = fake_engine(None);
    engine.send(queue(&["a.flac"]));
    engine.send(EngineCommand::Play);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });

    engine.send(EngineCommand::Pause);
    let paused = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Paused))
    });
    assert!(paused.iter().any(|e| matches!(e, EngineEvent::State(PlayState::Paused))));

    engine.send(EngineCommand::Play);
    let resumed = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });
    assert!(resumed.iter().any(|e| matches!(e, EngineEvent::State(PlayState::Playing))));

    engine.shutdown();
}

#[test]
fn queue_navigation_moves_and_stops_at_end() {
    let (engine, rx) = fake_engine(None);
    engine.send(queue(&["a.flac", "b.flac", "c.flac"]));
    engine.send(EngineCommand::PlayAt(1));

    let to_b = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::Track(info) if info.track.path == "b.flac")
    });
    assert_eq!(track_paths(&to_b), vec!["b.flac"]);

    engine.send(EngineCommand::Next);
    let to_c = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::Track(info) if info.track.path == "c.flac")
    });
    assert_eq!(track_paths(&to_c).last().map(String::as_str), Some("c.flac"));

    engine.send(EngineCommand::Next); // past the end
    let stopped = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });
    assert!(stopped.iter().any(|e| matches!(e, EngineEvent::State(PlayState::Stopped))));

    engine.shutdown();
}

#[test]
fn prev_restarts_first_track() {
    let (engine, rx) = fake_engine(None);
    engine.send(queue(&["a.flac", "b.flac"]));
    engine.send(EngineCommand::Play);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });

    engine.send(EngineCommand::Prev);
    let reload = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::Track(info) if info.track.path == "a.flac")
    });
    assert!(reload
        .iter()
        .any(|e| matches!(e, EngineEvent::Track(info) if info.track.path == "a.flac")));

    engine.shutdown();
}

#[test]
fn natural_end_of_queue_reaches_stopped() {
    let (engine, rx) = fake_engine(Some(4096));
    engine.send(queue(&["a.flac", "b.flac"]));
    engine.send(EngineCommand::Play);

    let events = collect_until(&rx, Duration::from_secs(3), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });

    assert_eq!(track_paths(&events), vec!["a.flac", "b.flac"]);
    // One Playing at the start, one Stopped at the very end — no churn between.
    assert_eq!(states(&events), vec![PlayState::Playing, PlayState::Stopped]);

    engine.shutdown();
}

#[test]
fn seek_reports_target_frame() {
    let (engine, rx) = fake_engine(None);
    engine.send(queue(&["a.flac"]));
    engine.send(EngineCommand::Play);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });

    engine.send(EngineCommand::Seek(500_000));
    let seeked = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::Position { frame } if *frame == 500_000)
    });
    assert!(
        seeked.iter().any(|e| matches!(e, EngineEvent::Position { frame } if *frame == 500_000)),
        "expected a position at the seek target: {seeked:?}"
    );

    engine.shutdown();
}

#[test]
fn set_queue_while_playing_stops() {
    let (engine, rx) = fake_engine(None);
    engine.send(queue(&["a.flac"]));
    engine.send(EngineCommand::Play);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });

    engine.send(queue(&["x.flac", "y.flac"]));
    let stopped = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });
    assert!(stopped.iter().any(|e| matches!(e, EngineEvent::State(PlayState::Stopped))));

    engine.shutdown();
}

#[test]
fn play_at_out_of_range_errors_without_panic() {
    let (engine, rx) = fake_engine(None);
    engine.send(queue(&["a.flac"]));
    engine.send(EngineCommand::PlayAt(9));

    let events = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::Error(_))
    });
    assert!(events.iter().any(|e| matches!(e, EngineEvent::Error(_))));

    engine.shutdown();
}

#[test]
fn command_storm_does_not_deadlock_or_panic() {
    let (engine, rx) = fake_engine(None);
    engine.send(queue(&["a.flac", "b.flac", "c.flac"]));

    let mut lcg: u64 = 0x1234_5678;
    for _ in 0..200 {
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let cmd = match (lcg >> 33) % 8 {
            0 => EngineCommand::Play,
            1 => EngineCommand::Pause,
            2 => EngineCommand::Stop,
            3 => EngineCommand::Next,
            4 => EngineCommand::Prev,
            5 => EngineCommand::Seek((lcg >> 10) % 1_000_000),
            6 => EngineCommand::PlayAt(((lcg >> 5) % 4) as usize),
            _ => queue(&["a.flac", "b.flac", "c.flac"]),
        };
        engine.send(cmd);
    }

    engine.send(EngineCommand::Stop);
    let _ = collect_until(&rx, Duration::from_millis(500), |_| false);
    engine.shutdown();
}

// --- Gapless / cue / seek-window tests -------------------------------------

/// A 5-track queue of same-spec tracks: the recorded device bytes equal the
/// byte-for-byte concatenation of each track's frames — no gap, no duplication at
/// any join (P3.3).
#[test]
fn gapless_same_spec_join_is_byte_exact() {
    let lens: [u64; 5] = [3000, 5000, 2048, 4096, 1500];
    let paths = ["t1", "t2", "t3", "t4", "t5"];
    let mut table = HashMap::new();
    let mut expected = Vec::new();
    for (p, &len) in paths.iter().zip(lens.iter()) {
        table.insert(
            tref(p, None),
            TrackSpec {
                image_key: (*p).into(),
                image_total: Some(len),
                virtual_start: 0,
                virtual_end: Some(len),
                spec: SPEC,
            },
        );
        expected.extend_from_slice(&pattern_bytes(SPEC, 0, len));
    }

    let (h, engine, rx) = Harness::build(table, None, true);
    engine.send(EngineCommand::SetQueue(paths.iter().map(|p| tref(p, None)).collect()));
    engine.send(EngineCommand::Play);

    let events = collect_until(&rx, Duration::from_secs(5), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });
    assert_eq!(track_paths(&events), paths, "each track announced in order");

    let recorded = h.recorder.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        expected.len(),
        "recorded byte count must equal the concatenation (no gap/dup)"
    );
    assert!(recorded == expected, "recorded device bytes must equal the unsplit concatenation");
    // The device opened exactly once — all five tracks shared one ring.
    assert_eq!(h.sink_opens.load(Ordering::Relaxed), 1, "same spec reuses one device");

    engine.shutdown();
}

/// A spec change between tracks drains the ring, re-opens the device, and emits a
/// new Format (P3.3).
#[test]
fn spec_change_drains_and_reopens_device() {
    let mut table = HashMap::new();
    table.insert(
        tref("a", None),
        TrackSpec { image_key: "a".into(), image_total: Some(2000), virtual_start: 0, virtual_end: Some(2000), spec: SPEC },
    );
    table.insert(
        tref("b", None),
        TrackSpec { image_key: "b".into(), image_total: Some(2000), virtual_start: 0, virtual_end: Some(2000), spec: SPEC_96 },
    );

    let (h, engine, rx) = Harness::build(table, None, false);
    engine.send(EngineCommand::SetQueue(vec![tref("a", None), tref("b", None)]));
    engine.send(EngineCommand::Play);

    let events = collect_until(&rx, Duration::from_secs(5), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });

    // Two Format events: 48 kHz then 96 kHz; the device re-opened for the change.
    let formats: Vec<StreamSpec> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Format { device, .. } => Some(*device),
            _ => None,
        })
        .collect();
    assert_eq!(formats, vec![StreamSpec::Pcm(SPEC), StreamSpec::Pcm(SPEC_96)], "{events:?}");
    assert_eq!(h.sink_opens.load(Ordering::Relaxed), 2, "spec change re-opens the device");
    assert_eq!(track_paths(&events), vec!["a", "b"]);

    engine.shutdown();
}

/// Adjacent virtual tracks of one cue image play gaplessly on a single decoder (no
/// re-open), and the recorded bytes equal the whole image (P3.4).
#[test]
fn cue_adjacent_virtual_tracks_reuse_one_decoder() {
    let image_total = 7000u64;
    let boundary = 3000u64;
    let mut table = HashMap::new();
    table.insert(
        tref("album.cue", Some(0)),
        TrackSpec { image_key: "img".into(), image_total: Some(image_total), virtual_start: 0, virtual_end: Some(boundary), spec: SPEC },
    );
    table.insert(
        tref("album.cue", Some(1)),
        TrackSpec { image_key: "img".into(), image_total: Some(image_total), virtual_start: boundary, virtual_end: Some(image_total), spec: SPEC },
    );

    let (h, engine, rx) = Harness::build(table, None, true);
    engine.send(EngineCommand::SetQueue(vec![tref("album.cue", Some(0)), tref("album.cue", Some(1))]));
    engine.send(EngineCommand::Play);

    let events = collect_until(&rx, Duration::from_secs(5), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });

    let recorded = h.recorder.lock().unwrap().clone();
    let whole = pattern_bytes(SPEC, 0, image_total);
    assert_eq!(recorded.len(), whole.len(), "cue playback length == whole image");
    assert!(recorded == whole, "cue-image playback must equal the unsplit whole image");
    // Only one decoder was opened for the whole image; the second virtual track
    // reused it across the join.
    assert_eq!(h.opens.load(Ordering::Relaxed), 1, "adjacent virtual tracks reuse one decoder");
    assert_eq!(h.sink_opens.load(Ordering::Relaxed), 1, "one device for the whole image");
    assert_eq!(track_paths(&events).len(), 2, "both virtual tracks announced");

    engine.shutdown();
}

/// A seek inside a virtual track clamps to the window and reports a position
/// relative to the virtual-track start (P3.4).
#[test]
fn seek_within_virtual_track_clamps_and_reports_relative() {
    // A long window (6.25 s at 48 kHz) so playback does not end before the seeks;
    // the track is carved out of an endless image.
    let vstart = 2_000u64;
    let vend = 302_000u64; // virtual length 300_000
    let mut table = HashMap::new();
    table.insert(
        tref("album.cue", Some(0)),
        TrackSpec { image_key: "img".into(), image_total: None, virtual_start: vstart, virtual_end: Some(vend), spec: SPEC },
    );

    let (_h, engine, rx) = Harness::build(table, None, false);
    engine.send(EngineCommand::SetQueue(vec![tref("album.cue", Some(0))]));
    engine.send(EngineCommand::Play);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });

    // A seek inside the window reports the relative frame.
    engine.send(EngineCommand::Seek(1_000));
    let within = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::Position { frame } if *frame == 1_000)
    });
    assert!(
        within.iter().any(|e| matches!(e, EngineEvent::Position { frame } if *frame == 1_000)),
        "seek inside the window reports the relative frame: {within:?}"
    );

    // A seek past the end clamps to length-1 (299_999), still relative.
    engine.send(EngineCommand::Seek(400_000));
    let clamped = collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::Position { frame } if *frame == 299_999)
    });
    assert!(
        clamped.iter().any(|e| matches!(e, EngineEvent::Position { frame } if *frame == 299_999)),
        "seek past the end clamps to the last frame of the window: {clamped:?}"
    );

    engine.shutdown();
}

// --- B1: flush the pre-seek/pre-stop tail -----------------------------------

/// B1 regression: a seek drops the pre-seek tail still buffered in the device ring
/// and the post-seek audio follows with no stale prefix. With a ring that only
/// plays bytes when the test consumes them, the pre-seek tail is observable; before
/// the worker flushed the sink on seek this tail would play before the landed
/// audio (invisible to the offline sink, audible on a real device).
#[test]
fn seek_flushes_pre_seek_tail_from_ring() {
    let bpf = SPEC.bytes_per_frame();
    let (state, flush_count, engine, rx) = ring_engine();
    engine.send(queue(&["a.flac"]));
    engine.send(EngineCommand::Play);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });

    // The worker fills the ring (nothing consumed yet); consume the first 1000
    // frames so they count as played. The rest of the ring is the pre-seek tail.
    assert!(
        wait_until(Duration::from_secs(1), || state.buffered_frames() >= 1000),
        "worker should fill the ring"
    );
    state.consume(1000);
    assert!(
        wait_until(Duration::from_secs(1), || state.buffered_frames() > 0),
        "a pre-seek tail must be buffered for this test to mean anything"
    );

    // Seek well past the buffered tail.
    engine.send(EngineCommand::Seek(500_000));
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::Position { frame } if *frame == 500_000)
    });

    // The seek flushed the ring (pre-seek tail dropped, never played).
    assert!(flush_count.load(Ordering::Relaxed) >= 1, "seek must flush the sink");

    // The worker refills from the seek target; consume and check the played stream
    // jumps from the 1000 pre-seek frames straight to frame 500_000 — none of the
    // dropped tail (which began at frame 1000) is in between.
    assert!(
        wait_until(Duration::from_secs(1), || state.buffered_frames() >= 1000),
        "worker should refill from the seek target"
    );
    state.consume(1000);
    let played = state.played_record();
    assert_eq!(
        &played[..1000 * bpf],
        &pattern_bytes(SPEC, 0, 1000)[..],
        "the first 1000 played frames are the pre-seek region"
    );
    let post = &played[1000 * bpf..];
    assert!(!post.is_empty(), "post-seek bytes were consumed");
    assert_eq!(
        post[0],
        (500_000u64 & 0xFF) as u8,
        "post-seek bytes follow with no stale pre-seek tail (B1)"
    );
    // The dropped tail began at frame 1000, whose pattern byte differs — prove it
    // is absent at the join.
    assert_ne!(post[0], (1000u64 & 0xFF) as u8, "the stale tail (frame 1000) must not play");

    engine.shutdown();
}

/// B1: Stop and SetQueue-while-playing flush the ring (device stays open), but
/// Pause does not — pause keeps the buffered audio so resume is seamless.
#[test]
fn stop_and_set_queue_flush_but_pause_does_not() {
    let (state, flush_count, engine, rx) = ring_engine();
    engine.send(queue(&["a.flac"]));
    engine.send(EngineCommand::Play);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });
    assert!(
        wait_until(Duration::from_secs(1), || state.buffered_frames() > 0),
        "worker should buffer audio"
    );

    // Pause must NOT flush (resume stays seamless).
    engine.send(EngineCommand::Pause);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Paused))
    });
    assert_eq!(flush_count.load(Ordering::Relaxed), 0, "pause must not flush");

    // Resume, then Stop must flush (device stays open).
    engine.send(EngineCommand::Play);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });
    engine.send(EngineCommand::Stop);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });
    // The worker handles Stop asynchronously (control emits Stopped first), so
    // wait for the flush to land on the worker thread.
    assert!(
        wait_until(Duration::from_secs(1), || flush_count.load(Ordering::Relaxed) >= 1),
        "stop must flush the ring"
    );
    let after_stop = flush_count.load(Ordering::Relaxed);

    // Play again, then SetQueue while playing must flush (it routes through Stop).
    engine.send(EngineCommand::Play);
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Playing))
    });
    assert!(
        wait_until(Duration::from_secs(1), || state.buffered_frames() > 0),
        "worker should buffer audio again"
    );
    engine.send(queue(&["x.flac", "y.flac"]));
    collect_until(&rx, Duration::from_secs(1), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });
    assert!(
        wait_until(Duration::from_secs(1), || flush_count.load(Ordering::Relaxed) > after_stop),
        "set-queue while playing must flush"
    );

    engine.shutdown();
}

// --- DSD→PCM feed rate (P8): the ring must never starve ----------------------

/// The device format the DSD→PCM path produces: 176.4 kHz, 24-bit, stereo.
const DSD_PCM_SPEC: PcmSpec =
    PcmSpec { rate: 176_400, channels: 2, bits_valid: 24, format: SampleFormat::S24 };

/// Output frames one decode read yields. Deliberately small to model the DSD→PCM
/// decimation: a DSD read feeds the FIR, which emits 16× fewer PCM frames, so one
/// "chunk" is tens of frames, not thousands. The *old* pump produced at most 8 such
/// chunks per 5 ms wake (`8 * 64` = 512 frames), fewer than the ~882 frames a
/// 176.4 kHz device drains per tick, so the ring starved periodically — the bug this
/// test pins. The fill-to-cushion pump matches the device's drain rate regardless of
/// per-read output size.
///
/// The decode/convert *cost* is modelled as negligible on purpose: the engine bug is
/// the feed-rate policy (how much the pump queues per wake), not converter CPU
/// throughput. The real FIR's throughput is a separate, release-measured concern
/// owned by `lyra-dsd` (and would make a debug unit test machine-dependent), so this
/// test exercises the pump policy with a cheap delivery that emits the same small
/// per-read output the decimation produces.
const CHUNK_FRAMES: u64 = 64;

/// An endless reader that emits `CHUNK_FRAMES` device-ready PCM frames per `read`,
/// modelling the small per-read output of the DSD→PCM decimation.
struct SmallChunkDecoder {
    spec: PcmSpec,
    tags: TagMap,
}

impl Decoder for SmallChunkDecoder {
    fn spec(&self) -> StreamSpec {
        StreamSpec::Pcm(self.spec)
    }
    fn total_frames(&self) -> Option<u64> {
        None // endless, so the real-time drain always has more to consume
    }
    fn seek(&mut self, frame: u64) -> Result<u64> {
        Ok(frame)
    }
    fn read(&mut self, out: &mut Vec<u8>) -> Result<usize> {
        let bytes = CHUNK_FRAMES as usize * self.spec.bytes_per_frame();
        out.resize(out.len() + bytes, 0);
        Ok(CHUNK_FRAMES as usize)
    }
    fn tags(&self) -> &TagMap {
        &self.tags
    }
}

/// A fake [`Source`] for a single processed (DSD→PCM-style) track whose decoder
/// yields the small per-read output the decimation produces.
struct SmallChunkSource;

impl Source for SmallChunkSource {
    fn resolve(&mut self, track: &TrackRef) -> Result<Resolved> {
        let source_spec = StreamSpec::Pcm(DSD_PCM_SPEC);
        let info = TrackInfo {
            track: track.clone(),
            title: None,
            artist: None,
            album: None,
            duration_frames: None,
            spec: source_spec,
        };
        Ok(Resolved {
            image_key: track.path.clone(),
            virtual_start: 0,
            virtual_end: None,
            source_spec,
            device_spec: source_spec,
            processed: true, // the DSD→PCM path is not device-bit-perfect (ADR-004)
            info,
        })
    }

    fn open(&mut self, _track: &TrackRef, _resolved: &Resolved) -> Result<Opened> {
        let decoder = Box::new(SmallChunkDecoder { spec: DSD_PCM_SPEC, tags: TagMap::new() });
        Ok(Opened { decoder, delivery: Delivery::Pcm, produced_pcm: Some(DSD_PCM_SPEC) })
    }
}

/// Startup grace: underruns before this are the unavoidable prefill transient owned
/// by `lyra_output`'s device prefill, not the steady feed-rate property under test.
const RT_GRACE: Duration = Duration::from_millis(200);
/// Ring depth, matching `lyra_output`'s ~2 s decode-side ring.
const RT_RING_SECS: u64 = 2;

/// A sink that models a real device: a bounded ring drained at the sample rate by
/// wall clock, with back-pressure on `push`, that **counts an underrun** each time a
/// drain finds the ring empty (the device wanted more frames than were pushed) after
/// the startup grace. This is the real-time-equivalent drain the P8 soak gate cares
/// about; the existing `MockSink` never counts underruns.
struct RtDrainSink {
    spec: StreamSpec,
    bpf: usize,
    rate: u64,
    ring_frames: u64,
    start: Instant,
    pushed: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
    starved: Arc<AtomicBool>,
}

impl RtDrainSink {
    /// Frames the device has tried to consume by now (real-time clock).
    fn wanted(&self) -> u64 {
        (self.start.elapsed().as_secs_f64() * self.rate as f64) as u64
    }
    /// Frames actually played (== wanted, capped by what has been pushed), counting a
    /// fresh underrun episode whenever the device out-runs the pushed audio.
    fn played_now(&self) -> u64 {
        let wanted = self.wanted();
        let pushed = self.pushed.load(Ordering::Relaxed);
        if self.start.elapsed() > RT_GRACE {
            if wanted > pushed {
                if !self.starved.swap(true, Ordering::Relaxed) {
                    self.underruns.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                self.starved.store(false, Ordering::Relaxed);
            }
        }
        wanted.min(pushed)
    }
}

impl AudioSink for RtDrainSink {
    fn negotiated(&self) -> StreamSpec {
        self.spec
    }
    fn is_bit_perfect(&self) -> bool {
        false // DSD→PCM is a processed path
    }
    fn push(&mut self, bytes: &[u8]) -> usize {
        let played = self.played_now();
        let pushed = self.pushed.load(Ordering::Relaxed);
        let in_ring = pushed.saturating_sub(played);
        let space = self.ring_frames.saturating_sub(in_ring);
        let offered = (bytes.len() / self.bpf) as u64;
        let accept = offered.min(space);
        self.pushed.fetch_add(accept, Ordering::Relaxed);
        accept as usize * self.bpf
    }
    fn flush(&mut self) {
        self.pushed.store(self.played_now(), Ordering::Relaxed);
    }
    fn frames_played(&self) -> u64 {
        self.played_now()
    }
    fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
    fn device_lost(&self) -> bool {
        false
    }
}

/// P8 regression: the DSD→PCM feed keeps the ring above empty at a real-time drain.
///
/// A processed 176.4 kHz track whose decoder emits only 64 frames per read (the
/// small output the 16× DSD→PCM decimation produces) is fed to a sink draining at the
/// sample rate. The previous fixed 8-chunk-per-tick pump produced ~512 frames per
/// 5 ms wake — fewer than the ~882 the device drains — and the ring starved. The
/// fill-to-cushion pump tops the ring up to a frame-depth target each wake, so the
/// feed keeps pace and no underrun is counted after the startup grace. (Before the
/// fix this counts non-zero underruns and fails.)
#[test]
fn dsd_to_pcm_feed_never_starves_the_ring() {
    let source: Box<dyn Source> = Box::new(SmallChunkSource);

    let underruns = Arc::new(AtomicU64::new(0));
    let underruns_c = underruns.clone();
    let sink = Box::new(move |spec: &StreamSpec| -> Result<Box<dyn AudioSink>> {
        let (bpf, rate) = match spec {
            StreamSpec::Pcm(p) => (p.bytes_per_frame(), u64::from(p.rate)),
            StreamSpec::Dsd(_) => return Err(LyraError::Device("DSD sink unsupported".into())),
        };
        Ok(Box::new(RtDrainSink {
            spec: *spec,
            bpf,
            rate,
            ring_frames: rate * RT_RING_SECS,
            start: Instant::now(),
            pushed: Arc::new(AtomicU64::new(0)),
            underruns: underruns_c.clone(),
            starved: Arc::new(AtomicBool::new(false)),
        }))
    });

    let (engine, rx) = Engine::with_factories(source, sink, TEST_PREROLL);
    engine.send(queue(&["beethoven.dff"]));
    engine.send(EngineCommand::Play);

    // Play well past the startup grace at real time, draining events meanwhile.
    collect_until(&rx, Duration::from_millis(900), |_| false);

    assert_eq!(
        underruns.load(Ordering::Relaxed),
        0,
        "DSD→PCM feed must keep the ring fed at real-time drain (0 underruns)"
    );

    engine.shutdown();
}

// --- P8 startup: open primes the ring before the device's first drain --------

/// A sink that never drains (`frames_played` stays 0), so a test can observe how
/// many frames the worker has queued *ahead of the device*. It accepts every pushed
/// byte and records the cumulative frame count — modelling the window between the
/// device opening (the feeder has prefilled its one silence buffer and started) and
/// its very first drain of real audio.
struct PrimeProbeSink {
    spec: StreamSpec,
    bpf: usize,
    pushed: Arc<AtomicU64>,
}

impl AudioSink for PrimeProbeSink {
    fn negotiated(&self) -> StreamSpec {
        self.spec
    }
    fn is_bit_perfect(&self) -> bool {
        false // DSD→PCM is a processed path
    }
    fn push(&mut self, bytes: &[u8]) -> usize {
        let whole = (bytes.len() / self.bpf) * self.bpf;
        self.pushed.fetch_add((whole / self.bpf) as u64, Ordering::Relaxed);
        whole
    }
    fn flush(&mut self) {}
    fn frames_played(&self) -> u64 {
        0 // the device has not begun draining real audio yet
    }
    fn underruns(&self) -> u64 {
        0
    }
    fn device_lost(&self) -> bool {
        false
    }
}

/// P8 startup regression: opening a processed DSD→PCM track primes the ring up to the
/// cushion **synchronously on `Load`**, before the device's first drain and without
/// waiting for the separate `Resume`.
///
/// `lyra_output`'s feeder prefills only one device buffer of silence and then starts
/// draining the ring the instant the device opens — inside `sink_factory`, before the
/// worker can push a frame. On the DSD→PCM path the first fill is the one expensive
/// pass (a whole-chunk FIR before any output frames exist); deferring it to a later
/// pump tick let that silence run out under CPU contention, giving exactly one startup
/// underrun on hardware. This drives the worker directly with `Load` **alone** (no
/// `Resume`) against a sink that has not begun draining (`frames_played` == 0), so the
/// only thing that can fill the ring is the synchronous prime inside `load`: the ring
/// must already hold at least the cushion.
///
/// Before the fix, the first fill waited for `Resume` plus a pump tick, so with `Load`
/// alone the ring stayed empty and this timed out; after the fix `Load` fills it at
/// once.
#[test]
fn open_primes_ring_to_cushion_before_first_drain() {
    use crate::worker::{self, WorkerCmd, WorkerEvt};

    let pushed = Arc::new(AtomicU64::new(0));
    let pushed_c = pushed.clone();
    let sink: crate::sink::SinkFactory = Box::new(move |spec: &StreamSpec| {
        let bpf = match spec {
            StreamSpec::Pcm(p) => p.bytes_per_frame(),
            StreamSpec::Dsd(_) => return Err(LyraError::Device("DSD sink unsupported".into())),
        };
        Ok(Box::new(PrimeProbeSink { spec: *spec, bpf, pushed: pushed_c.clone() })
            as Box<dyn AudioSink>)
    });
    let source: Box<dyn Source> = Box::new(SmallChunkSource);

    let (wevt_tx, _wevt_rx) = crossbeam_channel::unbounded::<WorkerEvt>();
    let (wcmd_tx, wcmd_rx) = crossbeam_channel::unbounded::<WorkerCmd>();
    let join = std::thread::Builder::new()
        .name("prime-probe-worker".into())
        .spawn(move || worker::run(wevt_tx, wcmd_rx, sink, source, TEST_PREROLL))
        .unwrap();

    // Load only — deliberately no Resume — so the sole thing that can fill the ring
    // is the synchronous prime inside `load`, not the per-wake pump (gated on play).
    wcmd_tx.send(WorkerCmd::Load(tref("beethoven.dff", None))).unwrap();

    // Cushion = FILL_TARGET (250 ms) worth of frames at the 176.4 kHz device rate.
    let cushion_frames = DSD_PCM_SPEC.rate as u64 * 250 / 1000;
    let primed = wait_until(Duration::from_secs(2), || {
        pushed.load(Ordering::Relaxed) >= cushion_frames
    });
    let got = pushed.load(Ordering::Relaxed);

    wcmd_tx.send(WorkerCmd::Shutdown).unwrap();
    join.join().unwrap();

    assert!(
        primed,
        "Load must prime the ring to the cushion ({cushion_frames} frames) before the \
         first device drain, independent of Resume; only {got} frames were queued"
    );
}
