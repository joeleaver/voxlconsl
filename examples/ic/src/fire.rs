//! Forest-fire spread, ported from big-world's `embers.rs` with two
//! twists for an RTS sim:
//!
//! 1. **Wind drift.** Every ember launches with a constant `(wx, wz)`
//!    bias on top of its random kick. The wind vector points toward
//!    the town, so the fire front advances on the player's objective
//!    if they don't intervene.
//! 2. **Cabin-flammable awareness.** When an ember lands on a cabin
//!    wall / roof voxel it ignites them (not just leaves / wood).
//!
//! The §10.3 CA still handles the slow cell-by-cell spread; embers
//! are how the fire jumps gaps — from forest to forest, or from
//! forest to a defensive perimeter and the town beyond.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics;

use crate::mathlib::{cosine, sine};
use crate::rng::Rng;
use crate::{
    M_CABIN_ROOF, M_CABIN_WOOD, M_EMBER, M_FIRE, M_PINE_LEAVES, M_PINE_WOOD,
};

// ── Tuning ────────────────────────────────────────────────────────

// Cap on simultaneously-tracked burn cells. Each tracked site does a
// material_at every tick + a restock set_voxel every CA cycle (~16
// frames), so the cost grows linearly with the cap. The previous
// 512 saturated fast enough that wavefront cells got cap-evicted
// before their burn TTL expired ("new trees burn in 2 s"). 1280
// fits a typical mid-fire forest comfortably; combined with the
// slower ca_thresholds and reduced WIND_SPREAD_RATE the fire grows
// gradually instead of saturating immediately. Hit-cap behaviour
// still replaces the worst-TTL slot for a graceful overflow.
const BURN_SITES_CAP:  usize = 1280;

// ── Cart-side burn duration ──────────────────────────────────────
//
// The §10.3 CA caps each fire cell's life at 15 ticks (4-bit field),
// so without help every voxel vanishes ~250 ms after ignition. The
// cart restocks `M_FIRE` from `tick()` while a per-site countdown is
// positive, decoupling the *visible* burn duration from the CA's
// hard cap. When the countdown hits zero we stop restocking and the
// CA evicts the cell to air within 15 frames.
//
// Per-material so cabins resist the fire long enough for engine
// crews to drive over and snuff them, while leaves go up fast.
//
// 60 fps tick rate; all values in frames.
const BURN_TICKS_PINE_LEAVES: u32 = 360;   // ~6 s
const BURN_TICKS_PINE_WOOD:   u32 = 600;   // ~10 s
const BURN_TICKS_CABIN_WOOD:  u32 = 900;   // ~15 s — most resistant
const BURN_TICKS_CABIN_ROOF:  u32 = 720;   // ~12 s
const BURN_TICKS_DEFAULT:     u32 = 720;   // ~12 s — fallback when
                                           //   we don't know the source
                                           //   material (CA propagation
                                           //   path).

#[inline]
fn burn_ttl_for_material(m: u8) -> u32 {
    match m {
        M_PINE_LEAVES => BURN_TICKS_PINE_LEAVES,
        M_PINE_WOOD   => BURN_TICKS_PINE_WOOD,
        M_CABIN_WOOD  => BURN_TICKS_CABIN_WOOD,
        M_CABIN_ROOF  => BURN_TICKS_CABIN_ROOF,
        _             => BURN_TICKS_DEFAULT,
    }
}
/// 1-in-N chance per site per tick to launch an ember. Lower = more
/// launches = faster long-distance jumps. Tested values:
///   - 20: fire self-extinguishes in ~10 s (broken baseline)
///   - 8:  some seeds catastrophic total-loss in 6 s (too aggressive)
///   - 15: tuned with short-lived burns + 256 cap; now over-produces
///         because long-burn keeps sites alive 6–15 s instead of 1 s,
///         so total embers/sec is ~10× higher → visually noisy
///   - 60: ~6 launches/frame at 512 saturated sites (was ~34/frame
///         at mod=15) — fewer ember streaks on screen, still seeds
///         long-distance jumps.
const SITE_LAUNCH_MOD: u32   = 60;

/// Pool size for in-flight embers. Sized to keep simultaneous
/// ember voxels small enough that the scene doesn't read as
/// confetti — 64 in-flight at peak is enough to telegraph "embers
/// are jumping" without dominating the frame.
const EMBERS_CAP:        usize = 64;
/// How many ticks an ember stays in the air before snuffing itself
/// out. 120 ticks ≈ 2 s of flight at 60 fps — long enough to cross
/// a one-tree gap, short enough that the screen doesn't accumulate
/// a drifting cloud of yellow voxels.
const EMBER_TTL_TICKS:   u32   = 120;
// Ember motion controls per-hop range, which sets first-cabin-loss
// time. Tested values:
//   - 0.40 / 0.60 wind → fire reaches town in ~5 s (too fast)
//   - 0.15 / 0.20 wind → fire never reaches town across 16 seeds (too slow)
//   - 0.25 / 0.35 wind → in-between, target first-loss around 30-60 s
const EMBER_VEL_XZ:      f32   = 0.25;
const EMBER_VEL_Y_MIN:   f32   = 0.55;
const EMBER_VEL_Y_MAX:   f32   = 1.20;
const EMBER_GRAVITY:     f32   = 0.040;

/// Max wind contribution to an ember's initial XZ velocity, at
/// strength = 1.0. 0.35 keeps strong wind meaningful (1.4× the base
/// velocity bias) without making embers teleport.
const WIND_MAX_SPEED:    f32   = 0.35;

/// Wind drifts on a slow clock so the player has time to read the
/// HUD between changes. ~3 s at the cart's ~17 fps tick rate.
const WIND_DRIFT_TICKS:  u32   = 50;
const WIND_ANGLE_JITTER: f32   = 0.30;       // ±rad per drift step
const WIND_STRENGTH_JITTER: f32 = 0.12;
const WIND_STRENGTH_MIN: f32   = 0.10;
const WIND_STRENGTH_MAX: f32   = 0.95;

/// Per-site, per-tick probability that a burn site directly ignites
/// its downwind cardinal neighbour at max strength. With BURN_SITES
/// at 512 and the cart-side long-burn loop holding cells lit for 6-15 s,
/// fast propagation saturates the cap and evicts wavefront cells
/// before they finish burning (visible as "newest trees burn fast").
/// 0.002 keeps wind direction meaningful but lets the CA's flammable
/// rule (slower, threshold-driven) do most of the propagation work.
const WIND_SPREAD_RATE:  f32   = 0.002;

// World bound checks share this — borrows the host's 512³ scene size.
const WORLD: u32 = 512;

// ── State ─────────────────────────────────────────────────────────

#[derive(Copy, Clone, Default)]
struct Ember {
    active:  bool,
    pos:     Vec3,
    vel:     Vec3,
    ttl:     u32,
    last:    UVec3,
    painted: bool,
}

pub(crate) struct FireState {
    burn_sites: [Option<(UVec3, u32)>; BURN_SITES_CAP],
    embers:     [Ember; EMBERS_CAP],
    rng:        Rng,
    /// Cached XZ wind vector derived from `wind_angle` + `wind_strength`
    /// every drift step. Embers add this to their initial velocity.
    wind:   Vec3,
    /// Direction the wind is *blowing toward*, in radians. `0` = north
    /// (toward -Z); `+π/2` = east (toward +X). Matches the
    /// camera-relative basis so a wind angle of 3π/4 (south-east) on
    /// the HUD shows embers drifting toward the bottom-right of the
    /// screen.
    wind_angle:    f32,
    wind_strength: f32,
    wind_tick:     u32,
    /// Frame counter for throttling work that doesn't need to run
    /// every tick (e.g. `discover_propagated_fire`).
    wind_tick_counter: u32,
}

impl FireState {
    pub(crate) const fn new() -> Self {
        // SE-bound wind aimed roughly at the town's heading. Each
        // mission starts here and drifts from this anchor.
        let wind_angle = 3.0 * core::f32::consts::FRAC_PI_4;
        let wind_strength = 0.4_f32;
        // wind_xz derived from angle/strength but core::f32::cos
        // isn't const, so the cached vector starts at zero and gets
        // filled on the first `tick`.
        Self {
            burn_sites: [None; BURN_SITES_CAP],
            embers: [Ember {
                active: false,
                pos: Vec3 { x: 0.0, y: 0.0, z: 0.0 },
                vel: Vec3 { x: 0.0, y: 0.0, z: 0.0 },
                ttl: 0,
                last: UVec3 { x: 0, y: 0, z: 0 },
                painted: false,
            }; EMBERS_CAP],
            rng: Rng(0xC0FF_EE17),
            wind: Vec3 { x: 0.0, y: 0.0, z: 0.0 },
            wind_angle,
            wind_strength,
            wind_tick: 0,
            wind_tick_counter: 0,
        }
    }

    /// Seed wind angle + strength from the active scenario. Called
    /// once after `scenario::init()` so the cart boots with the
    /// per-seed weather profile rather than the const default.
    pub(crate) fn apply_scenario(&mut self, s: &crate::scenario::Scenario) {
        self.wind_angle    = s.wind_angle_rad;
        self.wind_strength = s.wind_strength.clamp(WIND_STRENGTH_MIN, WIND_STRENGTH_MAX);
        self.refresh_wind_vec();
    }

    /// Update wind to a new angle + strength. Called by the season
    /// state machine when a new day starts and the weather rolls.
    pub(crate) fn set_wind(&mut self, angle_rad: f32, strength: f32) {
        self.wind_angle    = angle_rad;
        self.wind_strength = strength.clamp(WIND_STRENGTH_MIN, WIND_STRENGTH_MAX);
        self.wind_tick     = 0;
        self.refresh_wind_vec();
    }

    /// Current wind in voxels/tick (XZ plane only). Embers sample
    /// this at launch.
    pub(crate) fn wind(&self) -> Vec3 { self.wind }

    pub(crate) fn wind_angle_rad(&self) -> f32 { self.wind_angle }
    pub(crate) fn wind_strength(&self) -> f32 { self.wind_strength }

    /// 8-sector cardinal label for the wind's *blow-toward*
    /// direction. Quantised into 45° wedges aligned so the HUD's
    /// "S" lines up with screen-down for a north-up camera.
    pub(crate) fn wind_direction_label(&self) -> &'static str {
        let mut a = self.wind_angle;
        let tau = core::f32::consts::TAU;
        while a < 0.0 { a += tau; }
        while a >= tau { a -= tau; }
        let sector = ((a + core::f32::consts::FRAC_PI_8) / core::f32::consts::FRAC_PI_4) as u32;
        match sector % 8 {
            0 => "N",
            1 => "NE",
            2 => "E",
            3 => "SE",
            4 => "S",
            5 => "SW",
            6 => "W",
            7 => "NW",
            _ => "?",
        }
    }

    /// Strength as a single digit 0..9 for the sidebar readout.
    pub(crate) fn wind_strength_digit(&self) -> u32 {
        let clamped = self.wind_strength.clamp(0.0, 1.0);
        // `f32::round` isn't available in no_std without libm. Manual
        // round-half-to-even isn't worth the bytes; nearest-integer
        // via `+ 0.5` is good enough for a 0..9 display.
        ((clamped * 9.0) + 0.5) as u32
    }

    fn refresh_wind_vec(&mut self) {
        // `angle = 0` → north → wind blows toward -Z, so wz negative.
        // angle = π/2 → east → wx positive.
        let s = self.wind_strength.clamp(0.0, 1.0) * WIND_MAX_SPEED;
        let wx =  sine(self.wind_angle)   * s;
        let wz = -cosine(self.wind_angle) * s;
        self.wind = Vec3::new(wx, 0.0, wz);
    }

    fn tick_wind(&mut self) {
        self.wind_tick = self.wind_tick.wrapping_add(1);
        if self.wind_tick < WIND_DRIFT_TICKS && self.wind != Vec3::ZERO {
            return;
        }
        self.wind_tick = 0;
        // Nudge angle (signed) + strength (signed, clamped).
        let a_delta = self.rng.signed() * WIND_ANGLE_JITTER;
        let s_delta = self.rng.signed() * WIND_STRENGTH_JITTER;
        self.wind_angle += a_delta;
        self.wind_strength = (self.wind_strength + s_delta)
            .clamp(WIND_STRENGTH_MIN, WIND_STRENGTH_MAX);
        self.refresh_wind_vec();
    }

    /// Register a new burn site. `source_material` is the cell's
    /// material *before* it became M_FIRE — drives the per-material
    /// cart-side burn duration. Pass 0 (or any unknown slot) and the
    /// site uses `BURN_TICKS_DEFAULT`.
    ///
    /// Dedupes: if `pos` is already tracked, this is a no-op. Without
    /// it, callers that share neighbours (e.g. `discover_propagated_fire`
    /// scanning each tracked site's 6 neighbours) would create many
    /// entries pointing at the same world cell — the cap fills with
    /// dupes and propagated cells get pushed out before the cart can
    /// restock them. Critically: we do NOT refresh the existing TTL
    /// either — discovery re-scans every 4 frames, so a centrally-
    /// located cell (with many burn-site neighbours) would have its
    /// TTL constantly bumped back to default and never expire, while
    /// edge cells age normally and get evicted. Result: a single
    /// long-burning "centre" and a thin wavefront where everything
    /// else turns to ash fast. Burn duration is fixed at add time.
    pub(crate) fn add_burn_site(&mut self, pos: UVec3, source_material: u8) {
        let ttl = burn_ttl_for_material(source_material);
        // First scan: skip if pos is already tracked.
        for slot in self.burn_sites.iter() {
            if let Some((p, _)) = slot {
                if *p == pos { return; }
            }
        }
        // Second scan: empty slot, else evict the lowest-TTL entry.
        let mut worst_idx = 0usize;
        let mut worst_ttl = u32::MAX;
        for (i, slot) in self.burn_sites.iter_mut().enumerate() {
            match slot {
                None => { *slot = Some((pos, ttl)); return; }
                Some((_, t)) => {
                    if *t < worst_ttl { worst_ttl = *t; worst_idx = i; }
                }
            }
        }
        self.burn_sites[worst_idx] = Some((pos, ttl));
    }

    /// Drop any burn-site entry at `pos` (no voxel mutation). Used by
    /// `lib::extinguish_fire_cell` so a unit-driven snuff isn't
    /// auto-restocked by `tick()` on the next frame.
    pub(crate) fn drop_burn_site(&mut self, pos: UVec3) {
        for slot in self.burn_sites.iter_mut() {
            if let Some((p, _)) = slot {
                if *p == pos { *slot = None; }
            }
        }
    }

    /// Currently-tracked burn sites — drives the HUD's fire-front
    /// readout.
    pub(crate) fn burn_site_count(&self) -> u32 {
        let mut n = 0u32;
        for s in self.burn_sites.iter() { if s.is_some() { n += 1; } }
        n
    }

    fn launch_ember(&mut self, origin: Vec3) {
        let vx = self.rng.signed() * EMBER_VEL_XZ + self.wind.x;
        let vz = self.rng.signed() * EMBER_VEL_XZ + self.wind.z;
        let vy = EMBER_VEL_Y_MIN
            + self.rng.unit() * (EMBER_VEL_Y_MAX - EMBER_VEL_Y_MIN);
        for e in self.embers.iter_mut() {
            if e.active { continue; }
            *e = Ember {
                active:  true,
                pos:     origin,
                vel:     Vec3::new(vx, vy, vz),
                ttl:     EMBER_TTL_TICKS,
                last:    UVec3::new(0, 0, 0),
                painted: false,
            };
            return;
        }
        // All slots busy — drop silently.
    }

    /// Re-discover §10.3-propagated fire that the CA has spread into
    /// since the last tick. Without this, burn sites would die as
    /// the original ignition cell burnt out even though the fire is
    /// still alive next door.
    fn discover_propagated_fire(&mut self) {
        const NEIGHBOURS: [(i32, i32, i32); 6] = [
            (-1, 0, 0), (1, 0, 0),
            (0, -1, 0), (0, 1, 0),
            (0, 0, -1), (0, 0, 1),
        ];
        let mut known: [UVec3; BURN_SITES_CAP] = [UVec3 { x: 0, y: 0, z: 0 }; BURN_SITES_CAP];
        let mut known_count = 0usize;
        for slot in self.burn_sites.iter() {
            if let Some((p, _)) = slot {
                known[known_count] = *p;
                known_count += 1;
            }
        }
        for i in 0..known_count {
            let pos = known[i];
            for &(dx, dy, dz) in &NEIGHBOURS {
                let nx = (pos.x as i32 + dx).clamp(0, WORLD as i32 - 1) as u32;
                let ny = (pos.y as i32 + dy).clamp(0, WORLD as i32 - 1) as u32;
                let nz = (pos.z as i32 + dz).clamp(0, WORLD as i32 - 1) as u32;
                if physics::material_at(nx, ny, nz) != M_FIRE { continue; }
                let mut already = false;
                for k in 0..known_count {
                    if known[k].x == nx && known[k].y == ny && known[k].z == nz {
                        already = true;
                        break;
                    }
                }
                if !already {
                    // CA-propagated ignition: we only see the cell as
                    // M_FIRE now, the source material is already gone.
                    // Use the default TTL — it splits the difference
                    // between leaves and cabin wood.
                    self.add_burn_site(UVec3::new(nx, ny, nz), 0);
                }
            }
        }
    }

    /// One full tick: drift wind, discover newly propagated fire,
    /// run the wind-spread bias, roll ember launches, step every
    /// airborne ember.
    pub(crate) fn tick(&mut self) {
        self.tick_wind();
        // Discovery is N×6 material_at host crossings per call. The
        // §10.3 CA only spreads one cell per neighbour per tick, so a
        // missed-frame is at most one cell of latency — invisible to
        // the player. Running every 4th frame gives a 4× speedup on
        // this code path during active fires.
        self.wind_tick_counter = self.wind_tick_counter.wrapping_add(1);
        if self.wind_tick_counter % 4 == 0 {
            self.discover_propagated_fire();
        }
        self.wind_spread_step();

        // Roll each site. The §10.3 CA evicts each fire cell after 15
        // ticks (4-bit life cap); the cart restocks M_FIRE while the
        // per-site `ttl` is positive so the *visible* burn duration
        // matches the cart's tunable, not the host's hard cap. When
        // `ttl == 0` we stop restocking; the next CA tick (or two)
        // evicts the cell to air. An external overwrite (unit snuff,
        // water, retardant) shows up as a non-M_FIRE / non-zero
        // material and drops the site immediately.
        let sites_snapshot: [Option<(UVec3, u32)>; BURN_SITES_CAP] = self.burn_sites;
        for (idx, slot) in sites_snapshot.iter().enumerate() {
            if let Some((pos, ttl)) = *slot {
                if ttl == 0 {
                    if physics::material_at(pos.x, pos.y, pos.z) == M_FIRE {
                        set_voxel(pos, 0);
                    }
                    self.burn_sites[idx] = None;
                    continue;
                }
                let m = physics::material_at(pos.x, pos.y, pos.z);
                match m {
                    M_FIRE => {
                        self.burn_sites[idx] = Some((pos, ttl - 1));
                        if self.rng.next_u32() % SITE_LAUNCH_MOD == 0 {
                            let origin = Vec3::new(
                                pos.x as f32 + 0.5,
                                pos.y as f32 + 1.0,
                                pos.z as f32 + 0.5,
                            );
                            self.launch_ember(origin);
                        }
                    }
                    0 => {
                        set_voxel(pos, M_FIRE);
                        self.burn_sites[idx] = Some((pos, ttl - 1));
                    }
                    _ => {
                        self.burn_sites[idx] = None;
                    }
                }
            }
        }

        self.step_embers();
    }

    /// Once per tick, every burn site rolls for a chance to directly
    /// ignite its downwind cardinal neighbour. The chance scales with
    /// `wind_strength` and is weighted on `|wx|` vs `|wz|` so a wind
    /// at 45° (e.g. SE) splits its ignitions ~50/50 between +X and
    /// +Z cells. This is what makes a strong wind visibly shape the
    /// fire front into an oriented finger.
    fn wind_spread_step(&mut self) {
        let p = WIND_SPREAD_RATE * self.wind_strength;
        if p < 0.001 { return; }
        let wx = self.wind.x;
        let wz = self.wind.z;
        let abs_x = if wx < 0.0 { -wx } else { wx };
        let abs_z = if wz < 0.0 { -wz } else { wz };
        let total = abs_x + abs_z;
        if total < 0.05 { return; }

        let sites_snapshot: [Option<(UVec3, u32)>; BURN_SITES_CAP] = self.burn_sites;
        const NEW_CAP: usize = 32;
        let mut new_sites: [Option<(UVec3, u8)>; NEW_CAP] = [None; NEW_CAP];
        let mut new_count = 0usize;

        for slot in sites_snapshot.iter() {
            let pos = match *slot {
                Some((p, _)) => p,
                None => continue,
            };
            if physics::material_at(pos.x, pos.y, pos.z) != M_FIRE { continue; }
            if self.rng.unit() > p { continue; }

            // Pick which cardinal axis to spread on, weighted by
            // wind component magnitude.
            let r = self.rng.unit() * total;
            let (dx, dz) = if r < abs_x {
                (if wx >= 0.0 { 1 } else { -1 }, 0)
            } else {
                (0, if wz >= 0.0 { 1 } else { -1 })
            };

            let nx = (pos.x as i32 + dx).clamp(0, WORLD as i32 - 1) as u32;
            let nz = (pos.z as i32 + dz).clamp(0, WORLD as i32 - 1) as u32;
            let ny = pos.y;
            let m = physics::material_at(nx, ny, nz);
            // Only ignite flammables we know about. (The §10.3 CA's
            // `Material.ignites_to` field handles the actual conversion
            // for any FLAMMABLE-flagged slot, but we're skipping that
            // path and writing M_FIRE directly — so be explicit about
            // which slots the cart knows are flammable.)
            let flammable = m == M_PINE_LEAVES
                || m == M_PINE_WOOD
                || m == M_CABIN_WOOD
                || m == M_CABIN_ROOF;
            if !flammable { continue; }

            set_voxel(UVec3::new(nx, ny, nz), M_FIRE);
            if new_count < NEW_CAP {
                new_sites[new_count] = Some((UVec3::new(nx, ny, nz), m));
                new_count += 1;
            }
        }

        for i in 0..new_count {
            if let Some((p, m)) = new_sites[i] { self.add_burn_site(p, m); }
        }
    }

    fn step_embers(&mut self) {
        let mut new_sites: [Option<(UVec3, u8)>; EMBERS_CAP] = [None; EMBERS_CAP];
        let mut new_site_count = 0usize;

        for e in self.embers.iter_mut() {
            if !e.active { continue; }
            if e.painted { clear_ember_voxel(e.last); e.painted = false; }
            if e.ttl == 0 { e.active = false; continue; }
            e.ttl -= 1;

            e.pos = Vec3::new(e.pos.x + e.vel.x, e.pos.y + e.vel.y, e.pos.z + e.vel.z);
            e.vel = Vec3::new(e.vel.x, e.vel.y - EMBER_GRAVITY, e.vel.z);

            let xi = e.pos.x as i32;
            let yi = e.pos.y as i32;
            let zi = e.pos.z as i32;
            if xi < 0 || yi < 0 || zi < 0
                || xi >= WORLD as i32 || yi >= WORLD as i32 || zi >= WORLD as i32
            {
                e.active = false;
                continue;
            }
            let cell = UVec3::new(xi as u32, yi as u32, zi as u32);
            let m = physics::material_at(cell.x, cell.y, cell.z);

            // Ignites on any flammable hit (leaves, pine wood, cabin
            // wood, cabin roof). Drops a fire cell and births a new
            // burn site so the cart-side spread continues from there.
            if m == M_PINE_LEAVES || m == M_PINE_WOOD
                || m == M_CABIN_WOOD || m == M_CABIN_ROOF
            {
                set_voxel(cell, M_FIRE);
                if new_site_count < new_sites.len() {
                    new_sites[new_site_count] = Some((cell, m));
                    new_site_count += 1;
                }
                e.active = false;
                continue;
            }
            // Hit any other solid (terrain, firebreak, water) →
            // snuff. This is how water + firebreaks actually stop
            // the fire: an ember crossing the strip lands on
            // M_FIREBREAK_DIRT or M_WATER and dies without igniting.
            if m != 0 && m != M_EMBER {
                e.active = false;
                continue;
            }

            set_voxel(cell, M_EMBER);
            e.last = cell;
            e.painted = true;
        }

        for i in 0..new_site_count {
            if let Some((p, m)) = new_sites[i] { self.add_burn_site(p, m); }
        }
    }

}

fn clear_ember_voxel(p: UVec3) {
    if physics::material_at(p.x, p.y, p.z) == M_EMBER {
        set_voxel(p, 0);
    }
}
