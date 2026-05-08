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
pub mod audio;
pub mod input;
pub mod physics;
pub mod actors;
pub mod macro_grid;
pub mod prefabs;
pub mod runtime;
pub mod world;
pub mod sandbox;
