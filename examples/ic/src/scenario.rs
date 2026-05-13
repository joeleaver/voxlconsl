//! Per-mission RNG-driven world setup. v1 ships a deliberately
//! small surface — the forest scatter seed and the initial wind
//! profile — so that distinct cart boots already feel different
//! without paying the cost of a full terrain refactor. The struct
//! is shaped to grow: future fields (lake position, town anchor,
//! fire-seed origin, heightmap jitter, weather profile) slot in
//! without changing how the cart accesses them.
//!
//! All fields are derived from a single 32-bit `seed`. Passing the
//! same seed twice yields the same world.

use crate::mathlib::TAU_F32;
use crate::rng::Rng;

#[derive(Copy, Clone)]
pub(crate) struct Scenario {
    pub seed:           u32,
    pub tier:           u8,
    pub heli_count:     u8,
    pub crew_count:     u8,
    pub hotshot_count:  u8,
    pub engine_count:   u8,
    pub forest_rng:     u32,
    pub wind_angle_rad: f32,
    pub wind_strength:  f32,
}

impl Scenario {
    /// Map a difficulty tier (1.. arbitrary) to the unit budget the
    /// player starts with. Returns `(helis, crews, hotshot_crew_cap,
    /// engines)`. Each hot-shot order deploys a squad of
    /// `SQUAD_SIZE = 4` crews so the crew cap is a multiple of 4.
    pub(crate) fn budget_for_tier(tier: u8) -> (u8, u8, u8, u8) {
        match tier {
            0 | 1 => (0, 2, 0, 0),
            2     => (1, 2, 4, 1),  // 1 squad + 1 engine
            3     => (2, 3, 8, 2),  // 2 squads + 2 engines
            4     => (3, 4, 8, 3),
            _     => (3, 4, 8, 3),  // ceiling at MAX_ENGINES=4
        }
    }

    pub(crate) fn from_seed_and_tier(seed: u32, tier: u8) -> Self {
        // Mix the seed before pulling streams from it so adjacent
        // seeds (0, 1, 2, ...) produce visibly different worlds.
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9).wrapping_add(0x6543_2109));
        let forest_rng = rng.next_u32();

        // Initial wind direction in the eastern half of the compass
        // — SW through SE through NE — so embers always head
        // *roughly* toward the town in the south-east. Strength
        // biased medium (0.30..0.60); the cart's drift logic will
        // push it around from there.
        let arc_start = core::f32::consts::FRAC_PI_4;     // NE (π/4)
        let arc_span  = core::f32::consts::PI;             // sweeps to SW
        let mut wind_angle_rad = arc_start + rng.unit() * arc_span;
        while wind_angle_rad < 0.0      { wind_angle_rad += TAU_F32; }
        while wind_angle_rad >= TAU_F32 { wind_angle_rad -= TAU_F32; }
        let wind_strength = 0.30 + rng.unit() * 0.30;

        let (heli_count, crew_count, hotshot_count, engine_count) = Self::budget_for_tier(tier);

        Self {
            seed,
            tier,
            heli_count,
            crew_count,
            hotshot_count,
            engine_count,
            forest_rng,
            wind_angle_rad,
            wind_strength,
        }
    }
}

// ── Global access ────────────────────────────────────────────────
//
// The cart boots in `init()`, picks a seed, builds the Scenario, and
// stashes it here. Every module reads from it via `get()` after that.
// One global static keeps the wiring quiet — no need to thread
// `&Scenario` through every paint / spawn function.

static mut STATE: Option<Scenario> = None;

pub(crate) fn init(seed: u32, tier: u8) {
    unsafe { STATE = Some(Scenario::from_seed_and_tier(seed, tier)); }
}

pub(crate) fn get() -> &'static Scenario {
    unsafe {
        (&*(&raw const STATE))
            .as_ref()
            .expect("scenario not initialised — call scenario::init() first")
    }
}
