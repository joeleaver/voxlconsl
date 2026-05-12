//! Cart-local xorshift32 RNG. Determinism is nice for tuning scenes;
//! every system seeds its own stream so they don't interlock.

#[derive(Copy, Clone)]
pub(crate) struct Rng(pub u32);

impl Rng {
    pub(crate) fn new(seed: u32) -> Self {
        // Avoid the all-zeros fixed point.
        Self(if seed == 0 { 0xCAFEBABE } else { seed })
    }

    pub(crate) fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in `[0, n)`. `n > 0`.
    pub(crate) fn range(&mut self, n: u32) -> u32 { self.next_u32() % n }

    /// Uniform in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f32 {
        (self.next_u32() as f32) * (1.0 / 4_294_967_296.0)
    }

    /// Uniform in `[-1, 1)`.
    pub(crate) fn signed(&mut self) -> f32 {
        let r = self.next_u32() as i32;
        (r as f32) / (i32::MAX as f32)
    }
}
