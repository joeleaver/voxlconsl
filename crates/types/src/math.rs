//! Vector types used across the platform.
//!
//! Basic component-wise arithmetic + dot/cross/length helpers live here so
//! every consumer (host, SDK, bundler, tools) talks about vectors the same
//! way. More specialized math (matrices, transforms) belongs in the host
//! and SDK crates.

#[derive(Copy, Clone, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct UVec3 {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct IVec3 {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// 8-bit-per-axis vector. Used for actor-local coordinates (≤ 32³, fits in u8).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct U8Vec3 {
    pub x: u8,
    pub y: u8,
    pub z: u8,
}

impl Vec3 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0, z: 0.0 };
    pub const ONE: Self = Self { x: 1.0, y: 1.0, z: 1.0 };
    pub const X: Self = Self { x: 1.0, y: 0.0, z: 0.0 };
    pub const Y: Self = Self { x: 0.0, y: 1.0, z: 0.0 };
    pub const Z: Self = Self { x: 0.0, y: 0.0, z: 1.0 };

    pub const fn new(x: f32, y: f32, z: f32) -> Self { Self { x, y, z } }
    pub const fn splat(v: f32) -> Self { Self { x: v, y: v, z: v } }

    pub fn dot(self, b: Self) -> f32 { self.x * b.x + self.y * b.y + self.z * b.z }

    pub fn cross(self, b: Self) -> Self {
        Self {
            x: self.y * b.z - self.z * b.y,
            y: self.z * b.x - self.x * b.z,
            z: self.x * b.y - self.y * b.x,
        }
    }

    pub fn length_squared(self) -> f32 { self.dot(self) }

    pub fn length(self) -> f32 {
        // `libm` provides sqrt in no_std contexts.
        libm::sqrtf(self.length_squared())
    }

    pub fn normalize(self) -> Self {
        let len = self.length();
        if len > 0.0 { self * (1.0 / len) } else { Self::ZERO }
    }

    pub fn componentwise_recip(self) -> Self {
        Self { x: 1.0 / self.x, y: 1.0 / self.y, z: 1.0 / self.z }
    }

    pub fn min(self, b: Self) -> Self {
        Self { x: self.x.min(b.x), y: self.y.min(b.y), z: self.z.min(b.z) }
    }

    pub fn max(self, b: Self) -> Self {
        Self { x: self.x.max(b.x), y: self.y.max(b.y), z: self.z.max(b.z) }
    }
}

impl core::ops::Add for Vec3 {
    type Output = Self;
    fn add(self, b: Self) -> Self { Self::new(self.x + b.x, self.y + b.y, self.z + b.z) }
}
impl core::ops::Sub for Vec3 {
    type Output = Self;
    fn sub(self, b: Self) -> Self { Self::new(self.x - b.x, self.y - b.y, self.z - b.z) }
}
impl core::ops::Mul<f32> for Vec3 {
    type Output = Self;
    fn mul(self, s: f32) -> Self { Self::new(self.x * s, self.y * s, self.z * s) }
}
impl core::ops::Neg for Vec3 {
    type Output = Self;
    fn neg(self) -> Self { Self::new(-self.x, -self.y, -self.z) }
}

impl UVec3 {
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };
    pub const fn new(x: u32, y: u32, z: u32) -> Self { Self { x, y, z } }
}

impl IVec3 {
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };
    pub const fn new(x: i32, y: i32, z: i32) -> Self { Self { x, y, z } }
}

impl U8Vec3 {
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };
    pub const fn new(x: u8, y: u8, z: u8) -> Self { Self { x, y, z } }
}

#[cfg(feature = "bytemuck")]
mod bytemuck_impls {
    use super::*;
    use bytemuck::{Pod, Zeroable};

    unsafe impl Zeroable for Vec3 {}
    unsafe impl Pod for Vec3 {}
    unsafe impl Zeroable for UVec3 {}
    unsafe impl Pod for UVec3 {}
    unsafe impl Zeroable for IVec3 {}
    unsafe impl Pod for IVec3 {}
    unsafe impl Zeroable for U8Vec3 {}
    unsafe impl Pod for U8Vec3 {}
}
