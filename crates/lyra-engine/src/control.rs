//! The control thread: transport state machine, queue, and event emission.
//!
//! This thread is the single owner of the play queue and transport state, and the
//! single emitter of [`EngineEvent`]. It serialises two inputs with a
//! [`crossbeam_channel::select`]: [`ControlMsg`] from the [`crate::EngineHandle`]
//! and [`WorkerEvt`] from the decode worker. Because it only sends short messages
//! (never decodes or touches the device), commands are never blocked by I/O.

use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use lyra_core::{EngineCommand, EngineEvent, LyraError, PlayState};

use crate::worker::{WorkerCmd, WorkerEvt};

/// A message from the public handle to the control thread.
pub(crate) enum ControlMsg {
    /// A transport command from the backend.
    Cmd(EngineCommand),
    /// Tear everything down and join the worker.
    Shutdown,
}

/// Run the control loop until shutdown or a disconnected handle.
pub(crate) fn run(
    control_rx: Receiver<ControlMsg>,
    worker_rx: Receiver<WorkerEvt>,
    worker_tx: Sender<WorkerCmd>,
    event_tx: Sender<EngineEvent>,
    worker_join: JoinHandle<()>,
) {
    let mut control = Control {
        queue: Vec::new(),
        index: 0,
        state: PlayState::Stopped,
        pending_play: false,
        worker_tx,
        event_tx,
    };

    loop {
        crossbeam_channel::select! {
            recv(control_rx) -> msg => match msg {
                Ok(ControlMsg::Cmd(cmd)) => control.command(cmd),
                Ok(ControlMsg::Shutdown) | Err(_) => break,
            },
            recv(worker_rx) -> evt => match evt {
                Ok(evt) => control.worker_event(evt),
                // Worker gone unexpectedly: nothing more will play.
                Err(_) => control.set_state(PlayState::Stopped),
            },
        }
    }

    // Orderly shutdown: tell the worker to exit, then join it.
    control.worker_tx.send(WorkerCmd::Shutdown).ok();
    worker_join.join().ok();
}

/// Control-thread state.
struct Control {
    queue: Vec<lyra_core::TrackRef>,
    index: usize,
    state: PlayState,
    /// `true` while an open is in flight that should transition to `Playing`.
    pending_play: bool,
    worker_tx: Sender<WorkerCmd>,
    event_tx: Sender<EngineEvent>,
}

impl Control {
    fn command(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::SetQueue(queue) => {
                self.queue = queue;
                self.index = 0;
                if self.state != PlayState::Stopped {
                    self.worker_tx.send(WorkerCmd::Stop).ok();
                    self.pending_play = false;
                    self.set_state(PlayState::Stopped);
                }
            }
            EngineCommand::Play => match self.state {
                PlayState::Playing => {}
                PlayState::Paused => {
                    self.worker_tx.send(WorkerCmd::Resume).ok();
                    self.set_state(PlayState::Playing);
                }
                PlayState::Stopped => self.start_index(self.index),
            },
            EngineCommand::Pause => {
                if self.state == PlayState::Playing {
                    self.worker_tx.send(WorkerCmd::Pause).ok();
                    self.set_state(PlayState::Paused);
                }
            }
            EngineCommand::Stop => {
                self.worker_tx.send(WorkerCmd::Stop).ok();
                self.pending_play = false;
                self.set_state(PlayState::Stopped);
            }
            EngineCommand::Seek(frame) => {
                self.worker_tx.send(WorkerCmd::Seek(frame)).ok();
            }
            EngineCommand::Next => self.skip(1),
            EngineCommand::Prev => self.skip(-1),
            EngineCommand::PlayAt(i) => {
                if i < self.queue.len() {
                    self.start_index(i);
                } else {
                    self.emit(EngineEvent::Error(LyraError::InvalidState(format!(
                        "PlayAt({i}) out of range (queue len {})",
                        self.queue.len()
                    ))));
                }
            }
        }
    }

    fn worker_event(&mut self, evt: WorkerEvt) {
        match evt {
            WorkerEvt::Opened { info, source, device, bit_perfect } => {
                self.emit(EngineEvent::Track(info));
                self.emit(EngineEvent::Format { source, device, bit_perfect });
                if self.pending_play {
                    self.pending_play = false;
                    self.set_state(PlayState::Playing);
                }
            }
            WorkerEvt::OpenFailed(err) => {
                self.emit(EngineEvent::Error(err));
                self.on_track_failed();
            }
            WorkerEvt::Position(frame) => self.emit(EngineEvent::Position { frame }),
            WorkerEvt::TrackEnded => self.on_track_ended(),
            WorkerEvt::Underrun(count) => self.emit(EngineEvent::Underrun { count }),
            WorkerEvt::DeviceLost => {
                self.emit(EngineEvent::Error(LyraError::Device("device lost".into())));
                self.pending_play = false;
                self.set_state(PlayState::Stopped);
            }
            WorkerEvt::Error(err) => self.emit(EngineEvent::Error(err)),
        }
    }

    /// Load `index` and arrange to enter `Playing` once it opens.
    fn start_index(&mut self, index: usize) {
        if index >= self.queue.len() {
            self.worker_tx.send(WorkerCmd::Stop).ok();
            self.pending_play = false;
            self.set_state(PlayState::Stopped);
            return;
        }
        self.index = index;
        let track = self.queue[index].clone();
        self.worker_tx.send(WorkerCmd::Load(track)).ok();
        self.worker_tx.send(WorkerCmd::Resume).ok();
        self.pending_play = true;
    }

    /// Next/Prev movement within the queue.
    fn skip(&mut self, dir: isize) {
        if self.queue.is_empty() {
            return;
        }
        if dir < 0 && self.index == 0 {
            // Prev at the start restarts the first track.
            self.start_index(0);
            return;
        }
        let next = self.index as isize + dir;
        if next < 0 || next as usize >= self.queue.len() {
            self.worker_tx.send(WorkerCmd::Stop).ok();
            self.pending_play = false;
            self.set_state(PlayState::Stopped);
            return;
        }
        self.start_index(next as usize);
    }

    /// A track reached its end: advance, or stop at the end of the queue.
    fn on_track_ended(&mut self) {
        if self.index + 1 < self.queue.len() {
            self.start_index(self.index + 1);
        } else {
            self.worker_tx.send(WorkerCmd::Stop).ok();
            self.pending_play = false;
            self.set_state(PlayState::Stopped);
        }
    }

    /// A track failed to open: skip to the next, or stop if none remain.
    fn on_track_failed(&mut self) {
        if self.index + 1 < self.queue.len() {
            self.start_index(self.index + 1);
        } else {
            self.worker_tx.send(WorkerCmd::Stop).ok();
            self.pending_play = false;
            self.set_state(PlayState::Stopped);
        }
    }

    /// Emit a state change only when the state actually changes.
    fn set_state(&mut self, state: PlayState) {
        if self.state != state {
            self.state = state;
            self.emit(EngineEvent::State(state));
        }
    }

    fn emit(&self, event: EngineEvent) {
        self.event_tx.send(event).ok();
    }
}
