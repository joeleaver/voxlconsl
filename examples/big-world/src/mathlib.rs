//! Tiny no_std math helpers. Good to ~0.001 on `[-π, π]`.
//!
//! The cart is no_std + no_alloc, so libm isn't available without
//! pulling it in as a dependency. These polynomial approximations are
//! plenty for camera orbit / actor yaw / value-noise smoothing where
//! the absolute error is invisible.

use core::f32::consts::{FRAC_PI_2, PI, TAU};

pub(crate) fn sine(x: f32) -> f32 {
    let mut x = x % TAU;
    if x >  PI { x -= TAU; }
    if x < -PI { x += TAU; }
    let x2 = x * x;
    // Truncated Maclaurin series.
    x * (1.0 - x2 * (1.0 / 6.0 - x2 * (1.0 / 120.0 - x2 / 5040.0)))
}

pub(crate) fn cosine(x: f32) -> f32 { sine(x + FRAC_PI_2) }

pub(crate) fn atan2(y: f32, x: f32) -> f32 {
    if x == 0.0 && y == 0.0 { return 0.0; }
    let abs_x = if x < 0.0 { -x } else { x };
    let abs_y = if y < 0.0 { -y } else { y };
    let (a, swapped) = if abs_x > abs_y { (abs_y / abs_x, false) } else { (abs_x / abs_y, true) };
    let r = a * (0.97 - 0.19 * a * a);
    let r = if swapped { FRAC_PI_2 - r } else { r };
    let r = if x < 0.0 { PI - r } else { r };
    if y < 0.0 { -r } else { r }
}
