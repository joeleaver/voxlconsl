//! Cart-local xorshift32 — the SDK's host `rand` isn't wired yet, and
//! we only need enough randomness to pick a frightened-ghost direction
//! and scatter chomp-burst particles. Seeded deterministically: repeat
//! runs being reproducible is a feature, not a bug.

static mut STATE: u32 = 0xC0FF_EE17;

pub(crate) fn u32_() -> u32 {
    unsafe {
        let mut x = STATE;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        STATE = x;
        x
    }
}

/// `[0.0, 1.0)` float.
pub(crate) fn unit() -> f32 {
    (u32_() as f32) / (u32::MAX as f32 + 1.0)
}

/// `[-1.0, 1.0)` float.
pub(crate) fn signed() -> f32 {
    unit() * 2.0 - 1.0
}
