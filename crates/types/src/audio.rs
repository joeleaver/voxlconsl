//! Audio types — see SPEC.md §5.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PatchKind {
    Synth,
    Sampler,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum OscMode {
    Sine,
    Saw,
    SquarePwm,
    Triangle,
    Noise,
    Fm2op,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FilterMode {
    Off,
    Lp,
    Hp,
    Bp,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LfoShape {
    Sine,
    Tri,
    Square,
    /// Sample-and-hold.
    Sh,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LfoTarget {
    Pitch,
    Filter,
    Amp,
    Pan,
}

/// Returned by `sfx_play` so the cart can stop or modulate the voice later.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct VoiceId(pub u32);
