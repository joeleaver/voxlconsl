//! no_std sin/cos/sqrt helpers. Same polynomial approximations the
//! other example carts use — voxlconsl carts run `no_std` without
//! `libm`, so we roll our own to ~0.001 accuracy on `[-π, π]`.

use core::f32::consts::{FRAC_PI_2, PI, TAU};

pub(crate) const TAU_F32: f32 = TAU;

pub(crate) fn sine(x: f32) -> f32 {
    let mut x = x % TAU;
    if x >  PI { x -= TAU; }
    if x < -PI { x += TAU; }
    let x2 = x * x;
    x * (1.0 - x2 * (1.0 / 6.0 - x2 * (1.0 / 120.0 - x2 / 5040.0)))
}

pub(crate) fn cosine(x: f32) -> f32 { sine(x + FRAC_PI_2) }

/// `atan2(y, x)` to ~0.005 radians. Ported from big-world's mathlib.
pub(crate) fn atan2(y: f32, x: f32) -> f32 {
    if x == 0.0 && y == 0.0 { return 0.0; }
    let abs_x = if x < 0.0 { -x } else { x };
    let abs_y = if y < 0.0 { -y } else { y };
    let (a, swapped) = if abs_x > abs_y {
        (abs_y / abs_x, false)
    } else {
        (abs_x / abs_y, true)
    };
    let r = a * (0.97 - 0.19 * a * a);
    let r = if swapped { FRAC_PI_2 - r } else { r };
    let r = if x < 0.0 { PI - r } else { r };
    if y < 0.0 { -r } else { r }
}

/// Newton-Raphson sqrt; converges in ~4 iterations for our cell-scale
/// inputs. `x <= 0` returns 0.
pub(crate) fn sqrt(x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    let mut g = x * 0.5;
    for _ in 0..6 {
        g = 0.5 * (g + x / g);
    }
    g
}

