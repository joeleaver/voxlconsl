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

use crate::rng::Rng;
use crate::{
    M_CABIN_ROOF, M_CABIN_WOOD, M_EMBER, M_FIRE, M_PINE_LEAVES, M_PINE_WOOD,
};

// ── Tuning ────────────────────────────────────────────────────────

const BURN_SITES_CAP:  usize = 256;
const SITE_TTL_TICKS:  u32   = 480;
/// 1-in-N chance per site per tick to launch an ember.
const SITE_LAUNCH_MOD: u32   = 10;

const EMBERS_CAP:        usize = 96;
const EMBER_TTL_TICKS:   u32   = 320;
const EMBER_VEL_XZ:      f32   = 0.40;
const EMBER_VEL_Y_MIN:   f32   = 0.55;
const EMBER_VEL_Y_MAX:   f32   = 1.20;
const EMBER_GRAVITY:     f32   = 0.040;

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
    /// Wind direction in voxels-per-tick. Updated each tick — see
    /// `tick`. Always points toward the town in Phase 1; future
    /// scenarios can rotate it.
    pub wind:   Vec3,
}

impl FireState {
    pub(crate) const fn new() -> Self {
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
            wind: Vec3 { x: 0.15, y: 0.0, z: 0.20 },
        }
    }

    pub(crate) fn add_burn_site(&mut self, pos: UVec3) {
        let mut worst_idx = 0usize;
        let mut worst_ttl = u32::MAX;
        for (i, slot) in self.burn_sites.iter_mut().enumerate() {
            match slot {
                None => { *slot = Some((pos, SITE_TTL_TICKS)); return; }
                Some((_, ttl)) => {
                    if *ttl < worst_ttl { worst_ttl = *ttl; worst_idx = i; }
                }
            }
        }
        self.burn_sites[worst_idx] = Some((pos, SITE_TTL_TICKS));
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
                if !already { self.add_burn_site(UVec3::new(nx, ny, nz)); }
            }
        }
    }

    /// One full tick: discover newly propagated fire, roll launches,
    /// step every airborne ember.
    pub(crate) fn tick(&mut self) {
        self.discover_propagated_fire();

        // Roll each site.
        let sites_snapshot: [Option<(UVec3, u32)>; BURN_SITES_CAP] = self.burn_sites;
        for (idx, slot) in sites_snapshot.iter().enumerate() {
            if let Some((pos, ttl)) = *slot {
                if physics::material_at(pos.x, pos.y, pos.z) != M_FIRE {
                    self.burn_sites[idx] = None;
                    continue;
                }
                if ttl == 0 { self.burn_sites[idx] = None; continue; }
                self.burn_sites[idx] = Some((pos, ttl - 1));
                if self.rng.next_u32() % SITE_LAUNCH_MOD != 0 { continue; }
                let origin = Vec3::new(
                    pos.x as f32 + 0.5,
                    pos.y as f32 + 1.0,
                    pos.z as f32 + 0.5,
                );
                self.launch_ember(origin);
            }
        }

        self.step_embers();
    }

    fn step_embers(&mut self) {
        let mut new_sites: [Option<UVec3>; EMBERS_CAP] = [None; EMBERS_CAP];
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
                    new_sites[new_site_count] = Some(cell);
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
            if let Some(p) = new_sites[i] { self.add_burn_site(p); }
        }
    }

}

fn clear_ember_voxel(p: UVec3) {
    if physics::material_at(p.x, p.y, p.z) == M_EMBER {
        set_voxel(p, 0);
    }
}
