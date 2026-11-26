//! Engine construction: spawn the control and worker threads and wire the channels.

use std::time::Duration;

use crossbeam_channel::Receiver;
use lyra_core::{EngineEvent, Result};
use lyra_output::OutputOptions;

use crate::config::EngineConfig;
use crate::control::{self, ControlMsg};
use crate::handle::EngineHandle;
use crate::sink::{offline_sink_factory, wasapi_sink_factory, SinkFactory};
use crate::source::{real_source, SourceBox};
use crate::worker::{self, WorkerCmd, WorkerEvt};

/// The entry point to the playback engine.
///
/// [`Engine::new`] starts the engine and hands back an [`EngineHandle`] (to send
/// commands and shut down) and a [`Receiver`] of [`EngineEvent`] (state, position,
/// track, format, underruns, errors). The engine is otherwise opaque: all control
/// is through the handle, all observation through the event receiver.
pub struct Engine {
    _private: (),
}

impl Engine {
    /// Start the engine with the given configuration.
    ///
    /// Spawns the control and decode-worker threads. The output device is **not**
    /// opened until a track plays, so construction succeeds even with no endpoint
    /// available (opening then surfaces as an [`EngineEvent::Error`]).
    ///
    /// # Errors
    /// Currently infallible for the real backend, but returns [`Result`] so future
    /// eager validation (e.g. device selection) can report a failure here.
    // The `(handle, receiver)` return is the S05 contract, not `Self`.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(config: EngineConfig) -> Result<(EngineHandle, Receiver<EngineEvent>)> {
        let opts = OutputOptions {
            tap_path: config.tap_path.clone(),
            allow_poll_fallback: config.allow_poll_fallback,
        };
        let source = real_source(config.device.clone(), config.mode);
        let sink = match &config.offline {
            Some(path) => offline_sink_factory(path.clone())?,
            None => wasapi_sink_factory(config.device.clone(), config.mode, opts),
        };
        Ok(Self::spawn(source, sink, config.gapless_preroll))
    }

    /// Construct with injected source/sink factories (used by tests to drive the
    /// transport against in-memory fakes, no audio and no hardware).
    #[cfg(test)]
    pub(crate) fn with_factories(
        source: SourceBox,
        sink: SinkFactory,
        preroll: Duration,
    ) -> (EngineHandle, Receiver<EngineEvent>) {
        Self::spawn(source, sink, preroll)
    }

    /// Shared spawn path: create the channels and the two threads.
    fn spawn(
        source: SourceBox,
        sink: SinkFactory,
        preroll: Duration,
    ) -> (EngineHandle, Receiver<EngineEvent>) {
        let (event_tx, event_rx) = crossbeam_channel::unbounded::<EngineEvent>();
        let (control_tx, control_rx) = crossbeam_channel::unbounded::<ControlMsg>();
        let (wcmd_tx, wcmd_rx) = crossbeam_channel::unbounded::<WorkerCmd>();
        let (wevt_tx, wevt_rx) = crossbeam_channel::unbounded::<WorkerEvt>();

        let worker_join = std::thread::Builder::new()
            .name("lyra-decode-worker".into())
            .spawn(move || worker::run(wevt_tx, wcmd_rx, sink, source, preroll))
            .expect("spawn decode worker");

        let control_join = std::thread::Builder::new()
            .name("lyra-control".into())
            .spawn(move || control::run(control_rx, wevt_rx, wcmd_tx, event_tx, worker_join))
            .expect("spawn control thread");

        (EngineHandle::new(control_tx, control_join), event_rx)
    }
}
