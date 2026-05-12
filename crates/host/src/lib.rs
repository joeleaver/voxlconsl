//! voxlconsl host runtime.
//!
//! Module layout mirrors the spec sections so that "where does X live" is
//! always answerable by section number:
//!
//! | Module      | Spec section |
//! |-------------|--------------|
//! | `renderer`  | §3 Rendering |
//! | `palette`   | §4 Color |
//! | `audio`     | §5 Audio |
//! | `input`     | §6 Input |
//! | `physics`   | §10 Physics |
//! | `actors`    | §11 Actors |
//! | `runtime`   | cart loader, frame loop, save/load — cross-section glue |

pub mod renderer;
pub mod palette;
// §5 audio engine lives in its own crate (`voxlconsl-audio`) so the
// audio-worklet wasm can pull it in without dragging in the rest of
// the host. Re-export it under the historical `audio` name so the
// rest of the host code (and downstream tools) need no rename.
pub use voxlconsl_audio as audio;

/// Cart→audio event log written from sandbox.rs imports and drained
/// by the browser-host shim after every cart frame (SPEC.md §5.8).
pub mod audio_events;
pub mod input;
pub mod physics;
pub mod bodies;
pub mod ca;
pub mod actors;
pub mod macro_grid;
pub mod prefabs;
pub mod runtime;
pub mod world;
pub mod sandbox;
