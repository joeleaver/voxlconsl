//! Shared types for the voxlconsl fantasy console.
//!
//! Types here are used by the host, the SDK, and the bundler. They are the
//! lingua franca that lets cart code, tooling, and the runtime agree on what
//! a `Material` looks like, what an `ActionHandle` is, etc.
//!
//! Section references throughout point at `SPEC.md` at the workspace root.

#![no_std]

pub mod math;
pub mod material;
pub mod input;
pub mod camera;
pub mod actor;
pub mod physics;
pub mod audio;
pub mod cart_format;

pub use actor::*;
pub use audio::*;
pub use camera::*;
pub use input::*;
pub use material::*;
pub use math::*;
pub use physics::*;
