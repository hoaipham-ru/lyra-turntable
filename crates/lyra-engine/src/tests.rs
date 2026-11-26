//! In-crate transport/queue/clock tests driven by fake decoders and a fake sink.
//!
//! No real audio is rendered and no device is opened (the sound rule): the engine's
//! transport is exercised against an in-memory [`MockDecoder`] and [`MockSink`]
//! injected through [`Engine::with_factories`]. The sink consumes instantly, so
//! `frames_played` tracks pushed frames and a finite decoder reaches end-of-track
//! deterministically; an "endless" decoder keeps a track playing for navigation
//! and seek tests.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use lyra_core::{
    Decoder, EngineCommand, EngineEvent, LyraError, PcmSpec, PlayState, Result, SampleFormat,
    StreamSpec, TagMap, TrackInfo, TrackRef,
};

use crate::engine::Engine;
use crate::handle::EngineHandle;
use crate::sink::AudioSink;
use crate::source::{Delivery, OpenedSource};

const SPEC: PcmSpec = PcmSpec { rate: 48_000, channels: 2, bits_valid: 16, format: SampleFormat::S16 };

// --- Fakes -----------------------------------------------------------------

/// A finite or endless PCM decoder yielding zeroed frames.
struct MockDecoder {
    spec: StreamSpec,
    total: Option<u64>,
    pos: u64,
    chunk: u64,
    tags: TagMap,
}

impl MockDecoder {
    fn new(total: Option<u64>) -> Self {
        let mut tags = TagMap::new();
        tags.insert("TITLE", "Mock Title");
        tags.insert("ARTIST", "Mock Artist");
        Self { spec: StreamSpec::Pcm(SPEC), total, pos: 0, chunk: 4096, tags }
    }
}

impl Decoder for MockDecoder {
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
        self.pos += n;
        let bytes = n as usize * SPEC.bytes_per_frame();
        out.resize(out.len() + bytes, 0);
        Ok(n as usize)
    }
    fn tags(&self) -> &TagMap {
        &self.tags
    }
}

/// A fake sink that accepts every byte and consumes it instantly, so
/// `frames_played` equals the frames pushed so far.
struct MockSink {
    spec: StreamSpec,
    bpf: usize,
    played: Arc<AtomicU64>,
}

impl AudioSink for MockSink {
    fn negotiated(&self) -> StreamSpec {
        self.spec
    }
    fn is_bit_perfect(&self) -> bool {
        true
    }
    fn push(&mut self, bytes: &[u8]) -> usize {
        self.played.fetch_add((bytes.len() / self.bpf) as u64, Ordering::Relaxed);
        bytes.len()
    }
    fn frames_played(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }
    fn underruns(&self) -> u64 {
        0
    }
    fn device_lost(&self) -> bool {
        false
    }
}

/// Build an engine whose every track decodes `total` frames (`None` = endless).
fn fake_engine(total: Option<u64>) -> (EngineHandle, Receiver<EngineEvent>) {
    let source = Box::new(move |track: &TrackRef| -> Result<OpenedSource> {
        let decoder = Box::new(MockDecoder::new(total));
        let spec = StreamSpec::Pcm(SPEC);
        let info = TrackInfo {
            track: track.clone(),
            title: Some("Mock Title".into()),
            artist: Some("Mock Artist".into()),
            album: None,
            duration_frames: total,
            spec,
        };
        Ok(OpenedSource {
            decoder,
            info,
            source_spec: spec,
            device_spec: spec,
            delivery: Delivery::Pcm,
        })
    });
    let sink = Box::new(move |spec: &StreamSpec| -> Result<Box<dyn AudioSink>> {
        let bpf = match spec {
            StreamSpec::Pcm(p) => p.bytes_per_frame(),
            StreamSpec::Dsd(_) => 1,
        };
        Ok(Box::new(MockSink { spec: *spec, bpf, played: Arc::new(AtomicU64::new(0)) }))
    });
    Engine::with_factories(source, sink)
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

// --- Tests -----------------------------------------------------------------

#[test]
fn play_single_track_runs_to_stopped() {
    let (engine, rx) = fake_engine(Some(4096));
    engine.send(queue(&["a.flac"]));
    engine.send(EngineCommand::Play);

    let events = collect_until(&rx, Duration::from_secs(2), |e| {
        matches!(e, EngineEvent::State(PlayState::Stopped))
    });

    let seq = without_positions(&events);
    // Track, then Format, then State(Playing) at the front.
    assert!(matches!(seq[0], EngineEvent::Track(_)), "first: {seq:?}");
    assert!(matches!(seq[1], EngineEvent::Format { .. }), "second: {seq:?}");
    assert!(
        matches!(seq[2], EngineEvent::State(PlayState::Playing)),
        "third: {seq:?}"
    );
    assert_eq!(states(&events), vec![PlayState::Playing, PlayState::Stopped]);
    assert_eq!(track_paths(&events), vec!["a.flac"]);

    // A position of exactly the track length is reported before the end.
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
    let (engine, rx) = fake_engine(None); // endless tracks: only commands move us
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

    // Prev at index 0 reloads the first track (a new Track(a) event appears).
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
    let (engine, rx) = fake_engine(Some(4096)); // each track ends on its own
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

    // A small deterministic storm of out-of-order commands (the 1000-command
    // version is QA's P3.6); the engine must stay responsive.
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

    // The engine is still alive: a final Stop is honoured and shutdown joins.
    engine.send(EngineCommand::Stop);
    let _ = collect_until(&rx, Duration::from_millis(500), |_| false);
    engine.shutdown(); // must return (threads join) — no deadlock.
}
