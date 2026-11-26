//! S05 — the Lyra playback engine: transport, queue, position clock and routing.
//!
//! The engine ties decode (S02/S03) and output (S04) together behind a small
//! command/event surface. It owns two threads:
//!
//! * a **control thread** that receives [`EngineCommand`]s, holds the play queue
//!   and transport state, and emits [`EngineEvent`]s;
//! * a **decode worker thread** that opens the source decoder and the output
//!   device, fills the output ring, and reports playback position derived from the
//!   frames the device has actually consumed (never from frames decoded).
//!
//! Both threads talk over [`crossbeam_channel`] queues; the audio bytes travel
//! through the wait-free ring inside [`lyra_output`]. Commands are therefore never
//! blocked by decode I/O.
//!
//! ```no_run
//! use lyra_engine::{Engine, EngineConfig};
//! use lyra_core::{EngineCommand, TrackRef};
//!
//! let (engine, events) = Engine::new(EngineConfig::default())?;
//! engine.send(EngineCommand::SetQueue(vec![TrackRef::file("track.flac")]));
//! engine.send(EngineCommand::Play);
//! for event in events.iter().take(3) {
//!     println!("{event:?}");
//! }
//! engine.shutdown();
//! # Ok::<(), lyra_core::LyraError>(())
//! ```
//!
//! # Real-time rules
//! The engine itself never inspects or alters sample values; it only moves bytes
//! between the decoder/converter and the output ring. No allocation, locking or
//! logging happens on [`lyra_output`]'s real-time render thread — the engine feeds
//! that thread only through the lock-free ring from the decode worker.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

mod clock;
mod config;
mod control;
mod engine;
mod handle;
mod routing;
mod sink;
mod source;
mod worker;

#[cfg(test)]
mod tests;

pub use config::EngineConfig;
pub use engine::Engine;
pub use handle::EngineHandle;

// Re-exported for callers so a single `use lyra_engine::…` covers the command and
// event vocabulary. These are the `lyra-core` (S01) contract types, unchanged.
pub use lyra_core::{
    DeviceCaps, EngineCommand, EngineEvent, LyraError, OutputMode, PlayState, Result, StreamSpec,
    TrackInfo, TrackRef,
};
pub use lyra_output::DeviceId;
