//! The decode worker thread.
//!
//! The worker owns the current decoder, the DSD delivery state and the output
//! [`AudioSink`]. It opens tracks, fills the output ring (never blocking on the
//! control channel), and reports playback position from the frames the device has
//! consumed. It talks to the control thread over two [`crossbeam_channel`] queues:
//! it receives [`WorkerCmd`] and sends [`WorkerEvt`].
//!
//! Only the worker moves audio bytes, and it only *moves* them — padding into the
//! device container happens here (off the real-time render thread), never any
//! change to sample values on a bit-perfect path.

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use lyra_core::{Decoder, LyraError, PcmSpec, StreamSpec, TrackInfo, TrackRef};
use lyra_output::pad_frames;

use crate::clock::PositionClock;
use crate::sink::{AudioSink, SinkFactory};
use crate::source::{Delivery, SourceFactory};

/// How often the worker wakes to pump audio and poll stats.
const TICK: Duration = Duration::from_millis(5);
/// Minimum spacing between periodic position reports (~10×/second).
const POSITION_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound on decode chunks produced per wake, to keep the loop responsive.
const MAX_CHUNKS_PER_TICK: usize = 8;

/// A request from the control thread to the worker.
pub(crate) enum WorkerCmd {
    /// Open this track (reusing the device if the spec is unchanged).
    Load(TrackRef),
    /// Begin / continue filling the ring.
    Resume,
    /// Stop filling the ring but keep the device open.
    Pause,
    /// Stop playback and drop the current decoder; the device stays open.
    Stop,
    /// Seek the current decoder to this source frame.
    Seek(u64),
    /// Tear down and exit.
    Shutdown,
}

/// A notification from the worker to the control thread.
pub(crate) enum WorkerEvt {
    /// A track opened; the device format was negotiated.
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
    /// Opening a track failed.
    OpenFailed(LyraError),
    /// Current source-frame position.
    Position(u64),
    /// The current track decoded to EOS and the device drained it.
    TrackEnded,
    /// The cumulative underrun count rose to this value.
    Underrun(u64),
    /// The device was lost mid-stream.
    DeviceLost,
    /// A recoverable error occurred during playback.
    Error(LyraError),
}

/// Spawn-side worker state. Created on the worker thread by [`run`].
struct Worker {
    evt_tx: Sender<WorkerEvt>,
    cmd_rx: Receiver<WorkerCmd>,
    sink_factory: SinkFactory,
    source_factory: SourceFactory,

    sink: Option<Box<dyn AudioSink>>,
    current_device_spec: Option<StreamSpec>,
    dev_pcm: Option<PcmSpec>,

    decoder: Option<Box<dyn Decoder>>,
    delivery: Option<Delivery>,
    produced_pcm: Option<PcmSpec>,

    clock: PositionClock,
    total_frames: Option<u64>,
    seg_base_dev: u64,
    total_pushed_dev_frames: u64,

    playing: bool,
    eos: bool,

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
    source_factory: SourceFactory,
) {
    let mut worker = Worker {
        evt_tx,
        cmd_rx,
        sink_factory,
        source_factory,
        sink: None,
        current_device_spec: None,
        dev_pcm: None,
        decoder: None,
        delivery: None,
        produced_pcm: None,
        clock: PositionClock::new(1, 1),
        total_frames: None,
        seg_base_dev: 0,
        total_pushed_dev_frames: 0,
        playing: false,
        eos: false,
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
                    // Drain any other queued commands before pumping.
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
            self.pump();
            self.poll_stats();
            self.emit_position(false);
            self.check_track_end();
        }
    }

    /// Handle one command; returns `false` to stop the loop.
    fn handle(&mut self, cmd: WorkerCmd) -> bool {
        match cmd {
            WorkerCmd::Load(track) => self.load(&track),
            WorkerCmd::Resume => self.playing = true,
            WorkerCmd::Pause => self.playing = false,
            WorkerCmd::Stop => self.stop(),
            WorkerCmd::Seek(frame) => self.seek(frame),
            WorkerCmd::Shutdown => return false,
        }
        true
    }

    fn load(&mut self, track: &TrackRef) {
        let opened = match (self.source_factory)(track) {
            Ok(o) => o,
            Err(e) => {
                self.evt_tx.send(WorkerEvt::OpenFailed(e)).ok();
                return;
            }
        };
        let source_spec = opened.source_spec;
        let device_spec = opened.device_spec;
        let processed = opened.is_processed();
        let produced_pcm = opened.produced_pcm();

        // Reuse the open device when the spec is unchanged (gapless foundation);
        // otherwise drop the old stream and open a new one.
        let reuse = self.sink.is_some() && self.current_device_spec == Some(device_spec);
        if !reuse {
            self.sink = None; // release the old device before opening the next
            match (self.sink_factory)(&device_spec) {
                Ok(s) => {
                    self.sink = Some(s);
                    self.current_device_spec = Some(device_spec);
                }
                Err(e) => {
                    self.evt_tx.send(WorkerEvt::OpenFailed(e)).ok();
                    self.current_device_spec = None;
                    return;
                }
            }
        }

        let sink = self.sink.as_ref().expect("sink present after open");
        let negotiated = sink.negotiated();
        let dev_pcm = match negotiated {
            StreamSpec::Pcm(p) => p,
            StreamSpec::Dsd(_) => {
                self.evt_tx
                    .send(WorkerEvt::OpenFailed(LyraError::Device(
                        "device negotiated a DSD stream, unsupported in P3.1".into(),
                    )))
                    .ok();
                return;
            }
        };
        let bit_perfect = sink.is_bit_perfect() && !processed;
        let frames_played = sink.frames_played();

        self.dev_pcm = Some(dev_pcm);
        self.produced_pcm = produced_pcm;
        self.total_frames = opened.decoder.total_frames();
        self.decoder = Some(opened.decoder);
        self.delivery = Some(opened.delivery);

        let (src_hz, dev_hz) = segment_rates(source_spec, dev_pcm);
        self.clock = PositionClock::new(src_hz, dev_hz);
        self.seg_base_dev = frames_played;
        self.clock.rebase(0, frames_played);
        self.total_pushed_dev_frames = 0;
        self.eos = false;
        self.pending.clear();
        self.pending_off = 0;
        self.last_underruns = sink.underruns();
        self.device_lost_reported = false;

        self.evt_tx
            .send(WorkerEvt::Opened {
                info: opened.info,
                source: source_spec,
                device: negotiated,
                bit_perfect,
            })
            .ok();
        // Prime the clock with a position at the track start.
        self.emit_position(true);
    }

    fn stop(&mut self) {
        self.playing = false;
        self.eos = false;
        self.decoder = None;
        self.delivery = None;
        self.produced_pcm = None;
        self.pending.clear();
        self.pending_off = 0;
        self.total_pushed_dev_frames = 0;
    }

    fn seek(&mut self, frame: u64) {
        let landed = match self.decoder.as_mut() {
            Some(dec) => match dec.seek(frame) {
                Ok(f) => f,
                Err(e) => {
                    self.evt_tx.send(WorkerEvt::Error(e)).ok();
                    return;
                }
            },
            None => return,
        };
        if let Some(d) = self.delivery.as_mut() {
            d.reset();
        }
        self.pending.clear();
        self.pending_off = 0;
        self.eos = false;
        self.total_pushed_dev_frames = 0;
        let played = self.sink.as_ref().map_or(0, |s| s.frames_played());
        self.seg_base_dev = played;
        self.clock.rebase(landed, played);
        self.evt_tx.send(WorkerEvt::Position(landed)).ok();
        self.last_position_at = Instant::now();
    }

    /// Fill the ring with up to [`MAX_CHUNKS_PER_TICK`] chunks, honouring
    /// back-pressure (stop when the ring is full; never block).
    fn pump(&mut self) {
        if self.sink.is_none() {
            return;
        }
        if !self.flush_pending() {
            return; // ring still full: produce no more this tick
        }
        if !self.playing || self.eos || self.decoder.is_none() {
            return;
        }
        for _ in 0..MAX_CHUNKS_PER_TICK {
            self.in_buf.clear();
            let frames = match self.decoder.as_mut().unwrap().read(&mut self.in_buf) {
                Ok(n) => n,
                Err(e) => {
                    self.evt_tx.send(WorkerEvt::Error(e)).ok();
                    self.eos = true;
                    break;
                }
            };
            if frames == 0 {
                self.eos = true;
                break;
            }
            self.produce_into_pending();
            if !self.flush_pending() {
                break; // ring filled; carry the remainder
            }
        }
    }

    /// Convert `in_buf` to device-format bytes in `pending`, updating the pushed
    /// frame counter. Touches only disjoint fields, so no method re-borrow is made.
    fn produce_into_pending(&mut self) {
        let dev_format = self
            .dev_pcm
            .expect("dev_pcm set while playing")
            .format;
        match self.delivery.as_mut().expect("delivery set while playing") {
            Delivery::Pcm => {
                let src = self.produced_pcm.expect("produced spec for PCM");
                pad_frames(src, dev_format, &self.in_buf, &mut self.dev_buf);
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
        self.total_pushed_dev_frames += (self.dev_buf.len() / bpf) as u64;
        self.pending.extend_from_slice(&self.dev_buf);
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
            self.evt_tx.send(WorkerEvt::Underrun(underruns)).ok();
        }
        if sink.device_lost() && !self.device_lost_reported {
            self.device_lost_reported = true;
            self.evt_tx.send(WorkerEvt::DeviceLost).ok();
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
        if let Some(total) = self.total_frames {
            frame = frame.min(total);
        }
        self.evt_tx.send(WorkerEvt::Position(frame)).ok();
        self.last_position_at = Instant::now();
    }

    /// After EOS, once the device has played everything pushed, signal track end.
    fn check_track_end(&mut self) {
        if !self.eos || !self.pending.is_empty() {
            return;
        }
        let Some(sink) = self.sink.as_ref() else { return };
        let target = self.seg_base_dev.saturating_add(self.total_pushed_dev_frames);
        if sink.frames_played() >= target {
            // Report the exact end position, then the end-of-track.
            if let Some(total) = self.total_frames {
                self.evt_tx.send(WorkerEvt::Position(total)).ok();
            }
            self.decoder = None;
            self.delivery = None;
            self.produced_pcm = None;
            self.eos = false;
            self.evt_tx.send(WorkerEvt::TrackEnded).ok();
        }
    }
}

/// Source and device frame rates for the position clock.
fn segment_rates(source: StreamSpec, dev: PcmSpec) -> (u64, u64) {
    let src_hz = match source {
        StreamSpec::Pcm(p) => u64::from(p.rate),
        StreamSpec::Dsd(d) => u64::from(d.rate.hz()),
    };
    (src_hz, u64::from(dev.rate))
}
