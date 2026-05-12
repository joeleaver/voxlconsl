//! voxlconsl audio engine — pure DSP, no host or sandbox deps.
//!
//! Extracted from `voxlconsl-host` in v0.1.18 so it can be reused
//! by both the main host (running on the cart's thread) and the
//! audio-worklet wasm (running on the audio thread). See SPEC.md §5
//! for the spec and SPEC.md §5.8 for the threading model.

mod audio;
mod audio_fx;
mod audio_patch_blob;
mod smf;

pub use audio::*;
pub use audio_fx::{DelayState, ReverbState};
pub use audio_patch_blob::{
    load as patch_blob_load, save as patch_blob_save, PATCH_BLOB_MAX, PATCH_HEADER_BYTES,
    PATCH_ZONE_BYTES,
};
pub use smf::{parse as parse_smf, MidiEvent, SmfError, Song, TimedMidiEvent};
