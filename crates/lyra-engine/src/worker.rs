//! The decode worker thread.
//!
//! The worker owns the current decoder, the next-track pre-roll, the DSD delivery
//! state and the output [`AudioSink`]. It opens tracks, fills the output ring
//! (never blocking on the control channel), and reports playback position from the
//! frames the device has consumed. It talks to the control thread over two
//! [`crossbeam_channel`] queues: it receives [`WorkerCmd`] and sends [`WorkerEvt`].
//!
//! # Gapless (P3.3)
//! Control arms the worker with the next queue entry ([`WorkerCmd::PreloadNext`]).
//! The worker opens that decoder ahead of the current track's end (the pre-roll).
//! When the current track reaches its end the worker **appends** the next track's
//! frames to the same ring with no inserted silence when the device spec is
//! unchanged, or drains the ring and re-opens the device when the spec changes
//! (emitting [`WorkerEvt::Advanced`] either way). Two virtual tracks of one cue
//! image reuse the single open decoder across the join (no re-open, no re-seek when
//! contiguous).
//!
//! Only the worker moves audio bytes, and it only *moves* them — padding into the
//! device container happens here (off the real-time render thread), never any
//! change to sample values on a bit-perfect path.

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use lyra_core::{LyraError, PcmSpec, StreamSpec, TrackInfo, TrackRef};
use lyra_output::pad_frames;

use crate::clock::PositionClock;
use crate::sink::{AudioSink, SinkFactory};
use crate::source::{Delivery, Opened, Resolved, SourceBox};

/// How often the worker wakes to pump audio and poll stats.
const TICK: Duration = Duration::from_millis(5);
/// Minimum spacing between periodic position reports (~10×/second).
const POSITION_INTERVAL: Duration = Duration::from_millis(100);
/// Depth of audio the pump keeps buffered **ahead of the device**, as a wall-clock
/// duration of device output.
///
/// Each wake the pump tops the ring up to this cushion rather than stopping after a
/// fixed number of decode chunks. The cushion is sized so that neither the heavier
/// first fill on the DSD→PCM path (a whole-chunk FIR before any frames exist) nor a
/// transient converter-cost spike can drain the ring between 5 ms ticks: at
/// 176.4 kHz the device eats ~882 frames per tick, and 250 ms is ~44 100 frames of
/// slack. It stays well under `lyra_output`'s ~2 s ring, so production is bounded
/// and never fights back-pressure for the common case.
///
/// Replacing the old fixed 8-chunk cap matters for DSD→PCM because one decode read
/// yields 16× fewer output frames after decimation; a fixed small chunk budget
/// could produce fewer frames per tick than the device consumes and starve the ring
/// periodically. A frame-depth target keeps the feed rate matched to the device
/// regardless of the per-chunk output size.
const FILL_TARGET: Duration = Duration::from_millis(250);
/// Hard safety cap on produce iterations per wake. The real bound on per-tick work
/// is [`FILL_TARGET`] worth of frames; this only guards against a pathological
/// zero-output read loop (far above the chunk count any real fill needs).
const MAX_PRODUCE_ITERS_PER_TICK: usize = 4096;

/// A request from the control thread to the worker.
pub(crate) enum WorkerCmd {
    /// Open this track now (resetting any pre-roll; reusing the device if the spec
    /// is unchanged).
    Load(TrackRef),
    /// Arm the gapless pre-roll with the next queue entry, or `None` if the queue
    /// has no track after the current one (end after the current track).
    PreloadNext(Option<TrackRef>),
    /// Begin / continue filling the ring.
    Resume,
    /// Stop filling the ring but keep the device open.
    Pause,
    /// Stop playback and drop the current decoder; the device stays open.
    Stop,
    /// Seek the current (virtual) track to this frame, relative to its start.
    Seek(u64),
    /// Tear down and exit.
    Shutdown,
}

/// A notification from the worker to the control thread.
pub(crate) enum WorkerEvt {
    /// A track opened after an explicit load; the device format was negotiated.
    Opened {
        /// Resolved track info.
        info: TrackInfo,
        /// The native source format.
        source: StreamSpec,
        /// The negotiated device format.
        device: StreamSpec,
        /// `true` when samples reach the device unaltered.
        bit_perfect: bool,
    },
    /// The worker advanced to the pre-loaded next track at the gapless join.
    Advanced {
        /// Resolved info for the new track.
        info: TrackInfo,
        /// The native source format of the new track.
        source: StreamSpec,
        /// The negotiated device format (re-opened only when `spec_changed`).
        device: StreamSpec,
        /// `true` when samples reach the device unaltered.
        bit_perfect: bool,
        /// `true` when the device was drained and re-opened for a new spec.
        spec_changed: bool,
    },
    /// Opening a track failed.
    OpenFailed(LyraError),
    /// Current position, in frames relative to the (virtual) track start.
    Position(u64),
    /// The current track reached its end and the device drained it, with no next
    /// track pre-loaded.
    TrackEnded,
    /// The cumulative underrun count rose to this value.
    Underrun(u64),
    /// The device was lost mid-stream.
    DeviceLost,
    /// A recoverable error occurred during playback.
    Error(LyraError),
}

/// Pre-roll state for the next queue entry.
enum Preload {
    /// Nothing armed (end after the current track).
    None,
    /// Control named a next track; resolve it on the next tick.
    Pending(TrackRef),
    /// Resolved to a virtual track of the current image: reuse the open decoder at
    /// the join (no new open).
    Reuse { resolved: Resolved },
    /// Resolved to a different image: open its decoder once inside the pre-roll
    /// window.
    FreshPending { track: TrackRef, resolved: Resolved },
    /// Opened and ready to swap in at the join.
    Ready { resolved: Resolved, opened: Opened },
}

/// How a ring fill is driven.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fill {
    /// Steady-state per-wake top-up: cap work at one cushion of production so a wake
    /// never runs long; the next wake continues topping the ring up.
    Pump,
    /// Synchronous device-open / post-flush prime: keep filling until the ring truly
    /// holds a full cushion **ahead of the device**, so the device's first (or next,
    /// after a flush) drain finds real audio rather than the empty ring the flush
    /// left. A real device drains while the prime runs, so stopping after merely
    /// *producing* a cushion can still leave the ring under-filled (P8); this mode
    /// loops on `buffered_ahead` instead.
    Prime,
}

/// A spec change in flight: the ring is draining before the device re-opens.
struct Respec {
    resolved: Resolved,
    opened: Opened,
    /// Device frames that must play out before the re-open.
    target: u64,
}

/// Spawn-side worker state. Created on the worker thread by [`run`].
struct Worker {
    evt_tx: Sender<WorkerEvt>,
    cmd_rx: Receiver<WorkerCmd>,
    sink_factory: SinkFactory,
    source: SourceBox,
    preroll: Duration,

    sink: Option<Box<dyn AudioSink>>,
    current_device_spec: Option<StreamSpec>,
    dev_pcm: Option<PcmSpec>,

    // Current segment (flat, to keep delivery mutation and scratch buffers from
    // fighting the borrow checker).
    decoder: Option<Box<dyn lyra_core::Decoder>>,
    delivery: Option<Delivery>,
    produced_pcm: Option<PcmSpec>,
    source_spec: Option<StreamSpec>,
    image_key: String,
    virtual_start: u64,
    virtual_end: Option<u64>,
    src_pos: u64,
    length: Option<u64>,

    clock: PositionClock,
    /// Device frames-played value at which the current segment's frame 0 plays.
    seg_base_dev: u64,
    /// Device frames the ring has been filled to since the device opened (the
    /// append point for the next segment). Never reset while the device stays open.
    ring_end_dev: u64,

    preload: Preload,
    respec: Option<Respec>,

    playing: bool,
    eos: bool,

    /// Source bytes read past a virtual-track end, to hand to the next segment.
    carry: Vec<u8>,
    pending: Vec<u8>,
    pending_off: usize,
    in_buf: Vec<u8>,
    conv_buf: Vec<u8>,
    dev_buf: Vec<u8>,

    last_position_at: Instant,
    last_underruns: u64,
    device_lost_reported: bool,
}

/// Run the worker loop until [`WorkerCmd::Shutdown`] or a disconnected channel.
pub(crate) fn run(
    evt_tx: Sender<WorkerEvt>,
    cmd_rx: Receiver<WorkerCmd>,
    sink_factory: SinkFactory,
    source: SourceBox,
    preroll: Duration,
) {
    let mut worker = Worker {
        evt_tx,
        cmd_rx,
        sink_factory,
        source,
        preroll,
        sink: None,
        current_device_spec: None,
        dev_pcm: None,
        decoder: None,
        delivery: None,
        produced_pcm: None,
        source_spec: None,
        image_key: String::new(),
        virtual_start: 0,
        virtual_end: None,
        src_pos: 0,
        length: None,
        clock: PositionClock::new(1, 1),
        seg_base_dev: 0,
        ring_end_dev: 0,
        preload: Preload::None,
        respec: None,
        playing: false,
        eos: false,
        carry: Vec::new(),
        pending: Vec::new(),
        pending_off: 0,
        in_buf: Vec::new(),
        conv_buf: Vec::new(),
        dev_buf: Vec::new(),
        last_position_at: Instant::now(),
        last_underruns: 0,
        device_lost_reported: false,
    };
    worker.main_loop();
    // Teardown: dropping the sink stops the render/tap threads and releases the
    // device; dropping the decoder closes the file.
    worker.sink = None;
    worker.decoder = None;
    worker.delivery = None;
}

impl Worker {
    fn main_loop(&mut self) {
        loop {
            match self.cmd_rx.recv_timeout(TICK) {
                Ok(WorkerCmd::Shutdown) => break,
                Ok(cmd) => {
                    if !self.handle(cmd) {
                        break;
                    }
                    while let Ok(next) = self.cmd_rx.try_recv() {
                        if matches!(next, WorkerCmd::Shutdown) {
                            return;
                        }
                        if !self.handle(next) {
                            return;
                        }
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
            self.maybe_open_preload();
            self.pump();
            self.finish_respec();
            self.poll_stats();
            self.emit_position(false);
            self.check_track_end();
        }
    }

    /// Handle one command; returns `false` to stop the loop.
    fn handle(&mut self, cmd: WorkerCmd) -> bool {
        match cmd {
            WorkerCmd::Load(track) => self.load(&track),
            WorkerCmd::PreloadNext(next) => self.arm_preload(next),
            WorkerCmd::Resume => self.playing = true,
            WorkerCmd::Pause => self.playing = false,
            WorkerCmd::Stop => self.stop(),
            WorkerCmd::Seek(frame) => self.seek(frame),
            WorkerCmd::Shutdown => return false,
        }
        true
    }

    // --- Loading -----------------------------------------------------------

    fn load(&mut self, track: &TrackRef) {
        // An explicit load abandons any pre-roll or spec change in flight.
        self.preload = Preload::None;
        self.respec = None;
        self.carry.clear();

        let resolved = match self.source.resolve(track) {
            Ok(r) => r,
            Err(e) => {
                self.evt(WorkerEvt::OpenFailed(e));
                return;
            }
        };
        let opened = match self.source.open(track, &resolved) {
            Ok(o) => o,
            Err(e) => {
                self.evt(WorkerEvt::OpenFailed(e));
                return;
            }
        };

        // Reuse the open device when the spec is unchanged; otherwise re-open.
        let reuse = self.sink.is_some() && self.current_device_spec == Some(resolved.device_spec);
        if !reuse {
            self.sink = None; // release the old device before opening the next
            match (self.sink_factory)(&resolved.device_spec) {
                Ok(s) => {
                    self.sink = Some(s);
                    self.current_device_spec = Some(resolved.device_spec);
                }
                Err(e) => {
                    self.evt(WorkerEvt::OpenFailed(e));
                    self.current_device_spec = None;
                    return;
                }
            }
        } else {
            // An explicit (re)load onto the still-open device — Next/Prev/PlayAt, or
            // a restart — must drop the previous track's audio still queued toward the
            // device so the new track starts at once, not after the stale tail (B1).
            // The `prime_ring` below then refills the emptied ring from the new track,
            // so the device never drains into the post-flush gap (P8).
            self.flush_sink();
        }

        let sink = self.sink.as_ref().expect("sink present after open");
        let negotiated = sink.negotiated();
        let dev_pcm = match negotiated {
            StreamSpec::Pcm(p) => p,
            StreamSpec::Dsd(_) => {
                self.evt(WorkerEvt::OpenFailed(LyraError::Device(
                    "device negotiated a DSD stream, unsupported on the PCM path".into(),
                )));
                return;
            }
        };
        let bit_perfect = sink.is_bit_perfect() && !resolved.processed;
        // A fresh device restarts the append point; a reused device appends after
        // whatever is still queued.
        if !reuse {
            self.ring_end_dev = sink.frames_played();
        }

        self.dev_pcm = Some(dev_pcm);
        self.install_segment(opened, &resolved);

        let source = resolved.source_spec;
        let info = resolved.info;
        self.evt(WorkerEvt::Opened { info, source, device: negotiated, bit_perfect });
        self.emit_position(true);
        // Fill the ring to the cushion now, synchronously, before the device drains
        // its startup silence out from under the (expensive) first DSD→PCM fill.
        self.prime_ring();
    }

    /// Install `opened`/`resolved` as the current segment, rebasing the clock to
    /// the current append point.
    fn install_segment(&mut self, opened: Opened, resolved: &Resolved) {
        self.decoder = Some(opened.decoder);
        self.delivery = Some(opened.delivery);
        self.produced_pcm = opened.produced_pcm;
        self.source_spec = Some(resolved.source_spec);
        self.image_key = resolved.image_key.clone();
        self.virtual_start = resolved.virtual_start;
        self.virtual_end = resolved.virtual_end;
        self.src_pos = resolved.virtual_start;
        self.length = resolved.length();
        self.carry.clear();

        let dev_pcm = self.dev_pcm.expect("dev_pcm set");
        let (src_hz, dev_hz) = segment_rates(resolved.source_spec, dev_pcm);
        self.seg_base_dev = self.ring_end_dev;
        self.clock = PositionClock::new(src_hz, dev_hz);
        self.clock.rebase(0, self.ring_end_dev);
        self.eos = false;
        self.last_underruns = self.sink.as_ref().map_or(0, |s| s.underruns());
        self.device_lost_reported = false;
        self.last_position_at = Instant::now();
    }

    fn arm_preload(&mut self, next: Option<TrackRef>) {
        self.preload = match next {
            Some(track) => Preload::Pending(track),
            None => Preload::None,
        };
    }

    fn stop(&mut self) {
        self.playing = false;
        self.eos = false;
        self.decoder = None;
        self.delivery = None;
        self.produced_pcm = None;
        self.source_spec = None;
        self.preload = Preload::None;
        self.respec = None;
        self.carry.clear();
        self.pending.clear();
        self.pending_off = 0;
        // Drop audio still queued toward the device so a resume or a new queue
        // starts clean rather than replaying the stale tail (B1). A SetQueue
        // while playing routes through Stop, so this covers it too. The device
        // stays open; only its buffered bytes are discarded.
        self.flush_sink();
    }

    /// Discard audio still buffered toward the device — both the ring (via
    /// [`AudioSink::flush`]) and the worker's own un-pushed `pending` (cleared by
    /// the caller) — so the next bytes pushed are the next thing the device plays.
    ///
    /// Called on seek/stop/set-queue, never on pause (pause keeps the buffer so
    /// resume is seamless). Runs on the worker thread (may block up to the
    /// backend's flush timeout), never on a real-time thread.
    ///
    /// The discarded ring frames never advance `frames_played`, so the append
    /// point collapses onto the device's current played position: re-anchor
    /// `ring_end_dev` there so any clock rebase that follows maps the landed
    /// source frame to the very next frame the device will play.
    fn flush_sink(&mut self) {
        let Some(sink) = self.sink.as_mut() else { return };
        sink.flush();
        self.ring_end_dev = sink.frames_played();
    }

    fn seek(&mut self, frame: u64) {
        if self.decoder.is_none() {
            return;
        }
        // Clamp to the virtual-track window and map to an absolute source frame.
        let frame = match self.length {
            Some(len) if len > 0 => frame.min(len - 1),
            Some(_) => 0,
            None => frame,
        };
        let target_abs = self.virtual_start.saturating_add(frame);
        let landed_abs = match self.decoder.as_mut().unwrap().seek(target_abs) {
            Ok(f) => f,
            Err(e) => {
                self.evt(WorkerEvt::Error(e));
                return;
            }
        };
        if let Some(d) = self.delivery.as_mut() {
            d.reset();
        }
        self.carry.clear();
        self.pending.clear();
        self.pending_off = 0;
        self.eos = false;
        let landed_rel = landed_abs.saturating_sub(self.virtual_start);
        self.src_pos = landed_abs;
        // Drop the pre-seek tail still queued toward the device (ring + pending)
        // so the landed audio plays immediately instead of after the stale tail
        // (B1). `flush_sink` re-anchors `ring_end_dev` to the device's current
        // played position, so the rebase below stays correct.
        self.flush_sink();
        self.seg_base_dev = self.ring_end_dev;
        self.clock.rebase(landed_rel, self.ring_end_dev);
        self.evt(WorkerEvt::Position(landed_rel));
        self.last_position_at = Instant::now();
        // Re-fill the ring the flush just emptied, synchronously, before returning to
        // the main loop — otherwise the running device drains into the empty ring in
        // the window before the next pump tick, which under CPU contention is one (or
        // several) underruns per seek (P8). Only while playing: a paused seek leaves
        // the ring empty by design (the next Resume pumps it), and the small added
        // seek latency is bounded by the cushion depth (correctness over snappy).
        if self.playing {
            self.prime_ring();
        }
    }

    // --- Pre-roll ----------------------------------------------------------

    /// Resolve a pending pre-load, and open a fresh one once inside the pre-roll
    /// window (or when `force`d at the join).
    fn ensure_preload(&mut self, force: bool) {
        if let Preload::Pending(track) = &self.preload {
            let track = track.clone();
            match self.source.resolve(&track) {
                Ok(resolved) => {
                    self.preload = if self.can_reuse(&resolved) {
                        Preload::Reuse { resolved }
                    } else {
                        Preload::FreshPending { track, resolved }
                    };
                }
                Err(e) => {
                    self.evt(WorkerEvt::Error(e));
                    self.preload = Preload::None;
                    return;
                }
            }
        }

        if matches!(self.preload, Preload::FreshPending { .. }) && (force || self.within_preroll()) {
            let (track, resolved) = match std::mem::replace(&mut self.preload, Preload::None) {
                Preload::FreshPending { track, resolved } => (track, resolved),
                _ => unreachable!(),
            };
            match self.source.open(&track, &resolved) {
                Ok(opened) => self.preload = Preload::Ready { resolved, opened },
                Err(e) => {
                    self.evt(WorkerEvt::Error(e));
                    self.preload = Preload::None;
                }
            }
        }
    }

    /// Open the fresh pre-load lazily once the current track is near its end.
    fn maybe_open_preload(&mut self) {
        if self.decoder.is_some() && !self.eos {
            self.ensure_preload(false);
        }
    }

    /// Can the next track continue on the current decoder (same cue image)?
    fn can_reuse(&self, resolved: &Resolved) -> bool {
        self.decoder.is_some()
            && self.image_key == resolved.image_key
            && matches!(resolved.source_spec, StreamSpec::Pcm(_))
            && matches!(self.source_spec, Some(StreamSpec::Pcm(_)))
    }

    /// Are we within the pre-roll window of the current track's end?
    fn within_preroll(&self) -> bool {
        let Some(spec) = self.source_spec else { return true };
        let src_hz = source_hz(spec);
        let preroll_frames = (self.preroll.as_secs_f64() * src_hz as f64) as u64;
        match self.length {
            Some(len) => {
                let played = self.src_pos.saturating_sub(self.virtual_start);
                len.saturating_sub(played) <= preroll_frames
            }
            None => true,
        }
    }

    // --- Pumping -----------------------------------------------------------

    /// Per-wake pump: while playing, top the ring up to the cushion. Gated on
    /// `playing` so a paused track stops refilling (the already-buffered audio keeps
    /// draining — pause never flushes, so resume is seamless).
    fn pump(&mut self) {
        if !self.playing {
            return;
        }
        self.fill(Fill::Pump);
    }

    /// Synchronously pre-fill the ring up to the [`FILL_TARGET`] cushion at a device
    /// open, **before** returning to the main loop.
    ///
    /// The feeder thread inside `lyra_output` prefills only one device buffer of
    /// silence and then starts draining the ring the instant the device opens — which
    /// happens inside `sink_factory`, before the worker can push a single frame. On
    /// the DSD→PCM path the first fill is the one expensive pass (a whole-chunk FIR
    /// decimation must run before any output frames exist), so deferring it to the
    /// next main-loop `pump` tick let the device's one-buffer silence cushion run out
    /// under CPU contention — exactly one startup underrun (P8). Priming here closes
    /// that race: the first (incrementally pushed) frames land in the ring with no
    /// tick-cadence gap and without waiting for the separate [`WorkerCmd::Resume`], so
    /// the device's first post-start wake already finds real audio.
    ///
    /// Priming is unconditional (it does not check `playing`) because every
    /// control-initiated load is immediately followed by `Resume`, and the device is
    /// already draining regardless; a later `Pause` simply stops refilling and lets
    /// the primed audio drain out, exactly as pausing mid-playback does (pause never
    /// flushes). Device-open points call this — `load` (including the same-spec reuse
    /// re-load that first flushes the old track's tail) and the spec-change re-open in
    /// `finish_respec`; same-spec gapless joins keep one continuously-fed ring and need
    /// no prime. A seek re-primes too (while playing), after its flush, so the device
    /// does not drain into the empty ring the flush left (P8).
    fn prime_ring(&mut self) {
        self.fill(Fill::Prime);
    }

    /// Top the ring up to the [`FILL_TARGET`] cushion ahead of the device, crossing
    /// gapless joins in the same pass so no silence is inserted.
    ///
    /// The amount produced is governed by how far the ring has fallen below the
    /// cushion, not by a fixed chunk count, so the feed rate stays matched to the
    /// device however many output frames one decode read yields (the DSD→PCM path
    /// yields 16× fewer frames per read after decimation).
    ///
    /// Work stays bounded by [`MAX_PRODUCE_ITERS_PER_TICK`] and by `mode`:
    /// * [`Fill::Pump`] stops once the ring holds a cushion ahead of the device **or**
    ///   once this wake has produced a cushion's worth of frames — so a steady-state
    ///   wake never runs long, and an instantly-draining offline sink (which never
    ///   accumulates `buffered_ahead`) cannot run away.
    /// * [`Fill::Prime`] stops only once the ring *actually* holds a cushion ahead of
    ///   the device — a real device drains while the prime runs, so a produced-count
    ///   cap would under-fill it (P8). The offline sink never accumulates
    ///   `buffered_ahead`; there the same produced-count cap still applies (stop once a
    ///   cushion has been produced with nothing queued), so prime never hangs on it.
    fn fill(&mut self, mode: Fill) {
        if self.sink.is_none() || self.respec.is_some() {
            return;
        }
        // Never spin feeding a lost device: nothing drains it and the backend push is
        // a no-op; `poll_stats` surfaces the loss instead.
        if self.sink.as_deref().is_some_and(|s| s.device_lost()) {
            return;
        }
        if !self.flush_pending() {
            return;
        }
        let target = self.fill_target_frames();
        let mut produced: u64 = 0;
        for _ in 0..MAX_PRODUCE_ITERS_PER_TICK {
            if self.decoder.is_none() {
                return;
            }
            if self.eos {
                if !self.advance_at_eos() {
                    return; // end of queue (drain to TrackEnded) or spec change
                }
                continue; // new segment installed: produce it
            }
            // The ring holds a full cushion ahead of the device: done (both modes).
            if self.buffered_ahead() >= target {
                return;
            }
            match mode {
                // Steady-state wake: cap at one cushion of production so the wake
                // returns promptly; the next wake tops the ring back up. On an
                // instant-drain offline sink `buffered_ahead` never reaches the
                // cushion, so this cap is what ends the pass.
                Fill::Pump => {
                    if produced >= target {
                        return;
                    }
                }
                // Prime: keep filling until the ring truly holds a cushion ahead of the
                // draining device. Only an instant-drain sink (offline), where
                // `buffered_ahead` stays at 0, falls back to the produced-count cap so
                // the prime cannot loop forever — its missing cushion is produced by the
                // next pump pass, and the offline capture is continuous regardless.
                Fill::Prime => {
                    if produced >= target && self.buffered_ahead() == 0 {
                        return;
                    }
                }
            }
            // Source bytes: the carry from a crossed virtual-track boundary first,
            // then the decoder.
            if !self.carry.is_empty() {
                let carry = std::mem::take(&mut self.carry);
                self.in_buf.clear();
                self.in_buf.extend_from_slice(&carry);
            } else {
                self.in_buf.clear();
                let frames = match self.decoder.as_mut().unwrap().read(&mut self.in_buf) {
                    Ok(n) => n,
                    Err(e) => {
                        self.evt(WorkerEvt::Error(e));
                        self.eos = true;
                        continue;
                    }
                };
                if frames == 0 {
                    self.eos = true;
                    continue;
                }
            }
            let before = self.ring_end_dev;
            self.produce_into_pending();
            produced += self.ring_end_dev.saturating_sub(before);
            if !self.flush_pending() {
                break; // ring filled; carry the remainder next tick
            }
        }
    }

    /// The [`FILL_TARGET`] cushion expressed in device frames for the current output
    /// format (at least one frame, so the pump always makes progress while playing).
    fn fill_target_frames(&self) -> u64 {
        let rate = self.dev_pcm.map_or(0, |p| u64::from(p.rate));
        ((rate * FILL_TARGET.as_millis() as u64) / 1000).max(1)
    }

    /// Device frames currently queued ahead of the device: everything produced into
    /// the ring (and the worker's own un-pushed `pending`) that the device has not
    /// yet consumed. Derived from `frames_played`, so it reflects real draining.
    fn buffered_ahead(&self) -> u64 {
        let played = self.sink.as_ref().map_or(0, |s| s.frames_played());
        self.ring_end_dev.saturating_sub(played)
    }

    /// Convert `in_buf` (source bytes) to device bytes in `pending`, truncating at a
    /// virtual-track end and stashing the overflow in `carry`.
    fn produce_into_pending(&mut self) {
        let dev_format = self.dev_pcm.expect("dev_pcm set while playing").format;
        let mut overflow: Option<Vec<u8>> = None;
        match self.delivery.as_mut().expect("delivery set while playing") {
            Delivery::Pcm => {
                let src = self.produced_pcm.expect("produced spec for PCM");
                let bpf = src.bytes_per_frame().max(1);
                let frames_in = self.in_buf.len() / bpf;
                let allowed = match self.virtual_end {
                    Some(end) => end.saturating_sub(self.src_pos) as usize,
                    None => frames_in,
                };
                let use_frames = frames_in.min(allowed);
                let use_bytes = use_frames * bpf;
                pad_frames(src, dev_format, &self.in_buf[..use_bytes], &mut self.dev_buf);
                self.src_pos += use_frames as u64;
                if use_frames < frames_in {
                    overflow = Some(self.in_buf[use_bytes..].to_vec());
                }
                if let Some(end) = self.virtual_end {
                    if self.src_pos >= end {
                        self.eos = true;
                    }
                }
            }
            Delivery::DsdToPcm(conv) => {
                self.conv_buf.clear();
                conv.process(&self.in_buf, &mut self.conv_buf);
                let src = conv.out_spec();
                pad_frames(src, dev_format, &self.conv_buf, &mut self.dev_buf);
            }
            Delivery::Dop { packer, word_spec } => {
                self.conv_buf.clear();
                packer.pack(&self.in_buf, &mut self.conv_buf);
                pad_frames(*word_spec, dev_format, &self.conv_buf, &mut self.dev_buf);
            }
        }
        let bpf = self.dev_pcm.unwrap().bytes_per_frame().max(1);
        self.ring_end_dev += (self.dev_buf.len() / bpf) as u64;
        self.pending.extend_from_slice(&self.dev_buf);
        if let Some(c) = overflow {
            self.carry = c;
        }
    }

    /// At a track end, advance to the pre-loaded next track. Returns `false` when
    /// there is no gapless continuation (end of queue, or a spec-change drain).
    fn advance_at_eos(&mut self) -> bool {
        self.ensure_preload(true);
        match std::mem::replace(&mut self.preload, Preload::None) {
            Preload::None | Preload::Pending(_) | Preload::FreshPending { .. } => false,
            Preload::Reuse { resolved } => {
                self.swap_reuse(resolved);
                true
            }
            Preload::Ready { resolved, opened } => {
                if self.current_device_spec == Some(resolved.device_spec) {
                    self.swap_fresh(resolved, opened);
                    true
                } else {
                    self.begin_respec(resolved, opened);
                    false
                }
            }
        }
    }

    /// Gapless swap to a virtual track of the same image: keep the one decoder,
    /// seeking only if the next window is not contiguous.
    fn swap_reuse(&mut self, resolved: Resolved) {
        let need_seek = self.src_pos != resolved.virtual_start;
        if need_seek {
            if let Some(d) = self.decoder.as_mut() {
                if let Err(e) = d.seek(resolved.virtual_start) {
                    self.evt(WorkerEvt::Error(e));
                }
            }
            if let Some(d) = self.delivery.as_mut() {
                d.reset();
            }
            self.carry.clear();
        }
        // The decoder/delivery/produced_pcm are unchanged (same image, same spec).
        self.image_key = resolved.image_key.clone();
        self.virtual_start = resolved.virtual_start;
        self.virtual_end = resolved.virtual_end;
        self.src_pos = resolved.virtual_start;
        self.length = resolved.length();
        self.rebase_segment(resolved.source_spec);
        self.emit_advanced(&resolved, false);
    }

    /// Gapless swap to a different image on the same device spec: install the
    /// pre-opened decoder and keep appending to the same ring.
    fn swap_fresh(&mut self, resolved: Resolved, opened: Opened) {
        self.carry.clear();
        self.decoder = Some(opened.decoder);
        self.delivery = Some(opened.delivery);
        self.produced_pcm = opened.produced_pcm;
        self.image_key = resolved.image_key.clone();
        self.virtual_start = resolved.virtual_start;
        self.virtual_end = resolved.virtual_end;
        self.src_pos = resolved.virtual_start;
        self.length = resolved.length();
        self.rebase_segment(resolved.source_spec);
        self.emit_advanced(&resolved, false);
    }

    /// Begin a spec-change join: stop producing and wait for the ring to drain
    /// before the device re-opens.
    fn begin_respec(&mut self, resolved: Resolved, opened: Opened) {
        self.carry.clear();
        self.eos = true;
        self.respec = Some(Respec { resolved, opened, target: self.ring_end_dev });
    }

    /// Re-anchor the clock for a new same-device segment at the append point.
    fn rebase_segment(&mut self, source_spec: StreamSpec) {
        self.source_spec = Some(source_spec);
        let dev_pcm = self.dev_pcm.expect("dev_pcm set");
        let (src_hz, dev_hz) = segment_rates(source_spec, dev_pcm);
        self.seg_base_dev = self.ring_end_dev;
        self.clock = PositionClock::new(src_hz, dev_hz);
        self.clock.rebase(0, self.ring_end_dev);
        self.eos = false;
        self.last_position_at = Instant::now();
    }

    fn emit_advanced(&mut self, resolved: &Resolved, spec_changed: bool) {
        let sink = self.sink.as_ref().expect("sink present at advance");
        let bit_perfect = sink.is_bit_perfect() && !resolved.processed;
        let device = sink.negotiated();
        self.evt(WorkerEvt::Advanced {
            info: resolved.info.clone(),
            source: resolved.source_spec,
            device,
            bit_perfect,
            spec_changed,
        });
    }

    /// Once the ring has drained to the spec-change boundary, re-open the device
    /// and install the pending segment.
    fn finish_respec(&mut self) {
        let Some(respec) = self.respec.as_ref() else { return };
        if !self.pending.is_empty() {
            return;
        }
        let played = self.sink.as_ref().map_or(0, |s| s.frames_played());
        if played < respec.target {
            return;
        }
        let respec = self.respec.take().unwrap();

        self.sink = None; // drain complete: release the old device
        match (self.sink_factory)(&respec.resolved.device_spec) {
            Ok(s) => {
                self.sink = Some(s);
                self.current_device_spec = Some(respec.resolved.device_spec);
            }
            Err(e) => {
                self.evt(WorkerEvt::OpenFailed(e));
                self.current_device_spec = None;
                self.decoder = None;
                self.delivery = None;
                return;
            }
        }
        let sink = self.sink.as_ref().unwrap();
        let negotiated = sink.negotiated();
        let dev_pcm = match negotiated {
            StreamSpec::Pcm(p) => p,
            StreamSpec::Dsd(_) => {
                self.evt(WorkerEvt::OpenFailed(LyraError::Device(
                    "device negotiated a DSD stream, unsupported on the PCM path".into(),
                )));
                return;
            }
        };
        self.dev_pcm = Some(dev_pcm);
        self.ring_end_dev = sink.frames_played();
        self.install_segment(respec.opened, &respec.resolved);
        self.emit_advanced(&respec.resolved, true);
        self.emit_position(true);
        // Re-opening for a new spec is the same startup race as a fresh load: prime
        // the ring to the cushion before the re-opened device drains its silence.
        self.prime_ring();
    }

    /// Push as much of `pending` as the ring accepts. Returns `true` when `pending`
    /// is fully drained (so it is safe to produce more).
    fn flush_pending(&mut self) -> bool {
        if self.pending_off >= self.pending.len() {
            self.pending.clear();
            self.pending_off = 0;
            return true;
        }
        let sink = self.sink.as_mut().expect("sink present in flush");
        let accepted = sink.push(&self.pending[self.pending_off..]);
        self.pending_off += accepted;
        if self.pending_off >= self.pending.len() {
            self.pending.clear();
            self.pending_off = 0;
            true
        } else {
            false
        }
    }

    fn poll_stats(&mut self) {
        let Some(sink) = self.sink.as_ref() else { return };
        let underruns = sink.underruns();
        if underruns > self.last_underruns {
            self.last_underruns = underruns;
            self.evt(WorkerEvt::Underrun(underruns));
        }
        if sink.device_lost() && !self.device_lost_reported {
            self.device_lost_reported = true;
            self.evt(WorkerEvt::DeviceLost);
        }
    }

    /// Emit a position report, throttled to [`POSITION_INTERVAL`] unless `force`.
    fn emit_position(&mut self, force: bool) {
        if self.decoder.is_none() || self.sink.is_none() {
            return;
        }
        if !self.playing && !force {
            return;
        }
        if !force && self.last_position_at.elapsed() < POSITION_INTERVAL {
            return;
        }
        let played = self.sink.as_ref().unwrap().frames_played();
        let mut frame = self.clock.source_frame(played);
        if let Some(len) = self.length {
            frame = frame.min(len);
        }
        self.evt(WorkerEvt::Position(frame));
        self.last_position_at = Instant::now();
    }

    /// After a track's end with no gapless continuation, once the device has played
    /// everything pushed, signal end-of-track.
    fn check_track_end(&mut self) {
        if !self.eos || !self.pending.is_empty() || self.respec.is_some() {
            return;
        }
        if self.decoder.is_none() {
            return;
        }
        let Some(sink) = self.sink.as_ref() else { return };
        if sink.frames_played() >= self.ring_end_dev {
            if let Some(len) = self.length {
                self.evt(WorkerEvt::Position(len));
            }
            self.decoder = None;
            self.delivery = None;
            self.produced_pcm = None;
            self.source_spec = None;
            self.eos = false;
            self.carry.clear();
            self.evt(WorkerEvt::TrackEnded);
        }
    }

    fn evt(&self, event: WorkerEvt) {
        self.evt_tx.send(event).ok();
    }
}

/// Source and device frame rates for the position clock.
fn segment_rates(source: StreamSpec, dev: PcmSpec) -> (u64, u64) {
    (source_hz(source), u64::from(dev.rate))
}

/// Source frames per second for a spec (PCM rate, or DSD samples/channel per sec).
fn source_hz(source: StreamSpec) -> u64 {
    match source {
        StreamSpec::Pcm(p) => u64::from(p.rate),
        StreamSpec::Dsd(d) => u64::from(d.rate.hz()),
    }
}
