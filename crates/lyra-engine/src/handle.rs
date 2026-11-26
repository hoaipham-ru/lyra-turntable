//! The public engine handle: send commands, shut down cleanly.

use std::thread::JoinHandle;

use crossbeam_channel::Sender;
use lyra_core::EngineCommand;

use crate::control::ControlMsg;

/// The owner-side handle to a running engine.
///
/// Clone-free on purpose: there is one handle per engine. Send transport commands
/// with [`EngineHandle::send`]; tear the engine down with [`EngineHandle::shutdown`]
/// (which joins every thread and releases the device). Dropping the handle without
/// calling `shutdown` still shuts the engine down, but `shutdown` lets the caller
/// wait for threads to finish.
pub struct EngineHandle {
    control_tx: Sender<ControlMsg>,
    control_join: Option<JoinHandle<()>>,
}

impl EngineHandle {
    /// Construct from the control channel and the control thread's join handle.
    pub(crate) fn new(control_tx: Sender<ControlMsg>, control_join: JoinHandle<()>) -> Self {
        Self { control_tx, control_join: Some(control_join) }
    }

    /// Send a transport command. Commands are queued and handled in order; this
    /// call never blocks on decode or device I/O. A command sent after shutdown is
    /// silently dropped.
    pub fn send(&self, cmd: EngineCommand) {
        self.control_tx.send(ControlMsg::Cmd(cmd)).ok();
    }

    /// Stop the engine and wait for the control and worker threads to finish and
    /// the device to be released.
    pub fn shutdown(mut self) {
        self.teardown();
    }

    fn teardown(&mut self) {
        self.control_tx.send(ControlMsg::Shutdown).ok();
        if let Some(join) = self.control_join.take() {
            join.join().ok();
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.teardown();
    }
}
