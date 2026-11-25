//! S01 — Lyra Engine shared contracts.
//!
//! `lyra-core` is the contract crate every other Lyra crate depends on. It holds
//! the audio sample/stream descriptions, the [`Decoder`] and [`OutputDevice`]
//! traits, the engine command/event vocabulary, the metadata types and the
//! shared [`LyraError`]. It is deliberately free of I/O and platform code so the
//! bit-perfect audio path stays easy to audit; `std` is used, but nothing here
//! opens a file, a socket or a device.
//!
//! Any change to a public item in this crate is a contract change: it requires an
//! ADR entry, a minor version bump, and a list of affected crates in the handoff
//! report (see `services/S01-contracts.md`).
//!
//! # Versioning
//! This is version 0.1 of the contract. See the module-level docs for the exact
//! meaning of each field.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

mod command;
mod decoder;
mod device;
mod error;
mod event;
mod spec;
mod tags;
mod track;

pub use command::EngineCommand;
pub use decoder::Decoder;
pub use device::{DeviceCaps, OutputDevice, OutputMode};
pub use error::{LyraError, Result};
pub use event::{EngineEvent, PlayState};
pub use spec::{DsdRate, DsdSpec, Frames, PcmSpec, SampleFormat, StreamSpec};
pub use tags::TagMap;
pub use track::{TrackInfo, TrackRef};
