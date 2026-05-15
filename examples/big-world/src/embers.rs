//! Cart-side ember system that spreads fire across the forest.
//!
//! The §10.3 CA only spreads fire cell-by-cell to cardinal neighbours,
//! which is too slow to torch a forest from a single seed. This module
//! layers on top:
//!
//! 1. **Burn sites** — cart-tracked positions of cells currently on
//!    fire. Each tick, every site has a 1-in-N chance of launching an
//!    airborne ember.
//! 2. **Embers** — `M_EMBER` voxels with a velocity vector. Each tick
//!    the cart clears the ember's previous cell, advances by velocity,
//!    paints the new cell, and probes the destination material:
//!    - `M_LEAF` / `M_WOOD` → ignition (drop a fire voxel + new site)
//!    - other solid → snuff
//!    - air → keep flying
//!
//! Embers carry no CA flags, so they never enter the §10.3 active set
//! — the cart owns them top-to-bottom.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics;

use crate::{M_EMBER, M_FIRE, M_LEAF, M_WOOD, WORLD};
use crate::player::PLAYER_POS;
use crate::terrain::terrain_height;

// ── Tuning ────────────────────────────────────────────────────────

const BURN_SITES_CAP: usize = 128;
const SITE_TTL_TICKS: u32   = 360;
/// 1-in-N chance per site per tick to launch an ember.
const SITE_LAUNCH_MOD: u32  = 12;

const EMBERS_CAP:        usize = 64;
const EMBER_TTL_TICKS:   u32   = 240;
// Initial-velocity scales. xz components are signed in
// `[-EMBER_VEL_XZ, +EMBER_VEL_XZ]`; y is biased upward in
// `[EMBER_VEL_Y_MIN, EMBER_VEL_Y_MAX]` so embers initially shoot up
// before gravity arcs them back down.
const EMBER_VEL_XZ:    f32 = 0.45;
const EMBER_VEL_Y_MIN: f32 = 0.55;
const EMBER_VEL_Y_MAX: f32 = 1.20;
const EMBER_GRAVITY:   f32 = 0.040;

// ── State ─────────────────────────────────────────────────────────

#[derive(Copy, Clone, Default)]
struct Ember {
    active: bool,
    pos:    Vec3,
    vel:    Vec3,
    ttl:    u32,
    /// Last cell the ember was painted into (so we can clear it
    /// before painting the next). `painted == false` means we
    /// haven't drawn this ember yet (first tick of its life).
    last:    UVec3,
    painted: bool,
}

static mut BURN_SITES: [Option<(UVec3, u32)>; BURN_SITES_CAP] = [None; BURN_SITES_CAP];
static mut EMBERS:     [Ember; EMBERS_CAP] = [Ember {
    active: false, pos: Vec3 { x: 0.0, y: 0.0, z: 0.0 }, vel: Vec3 { x: 0.0, y: 0.0, z: 0.0 },
    ttl: 0, last: UVec3 { x: 0, y: 0, z: 0 }, painted: false,
}; EMBERS_CAP];

// ── RNG ──────────────────────────────────────────────────────────

static mut EMBER_RNG: u32 = 0xC0FF_EEBA;

fn ember_rand() -> u32 {
    unsafe {
        EMBER_RNG = EMBER_RNG.wrapping_mul(0x6C8E_9CF5).wrapping_add(0x9E37_79B9);
        EMBER_RNG
    }
}

fn rand_signed() -> f32 {
    let r = ember_rand();
    ((r as i32) as f32) / (i32::MAX as f32)
}

fn rand_unit() -> f32 {
    (ember_rand() as f32) / (u32::MAX as f32 + 1.0)
}

// ── Burn-site bookkeeping ────────────────────────────────────────

fn add_burn_site(pos: UVec3) {
    let sites = unsafe { &mut *(&raw mut BURN_SITES) };
    // Prefer an empty slot; otherwise overwrite the slot with the
    // smallest remaining TTL so a fresher site can take its place.
    let mut worst_idx = 0usize;
    let mut worst_ttl = u32::MAX;
    for (i, slot) in sites.iter_mut().enumerate() {
        match slot {
            None => { *slot = Some((pos, SITE_TTL_TICKS)); return; }
            Some((_, ttl)) => {
                if *ttl < worst_ttl { worst_ttl = *ttl; worst_idx = i; }
            }
        }
    }
    sites[worst_idx] = Some((pos, SITE_TTL_TICKS));
}

/// Pick a leaf or wood voxel near the player's spawn and ignite it.
/// Adds the position as the first burn site so embers start radiating
/// out from it.
pub(crate) fn seed_first_fire() {
    let px = unsafe { PLAYER_POS.x } as i32;
    let pz = unsafe { PLAYER_POS.z } as i32;
    for dy in 0..30 {
        for dz in -16i32..=16 {
            for dx in -16i32..=16 {
                let x = (px + dx).clamp(0, WORLD as i32 - 1) as u32;
                let z = (pz + dz).clamp(0, WORLD as i32 - 1) as u32;
                let y = (terrain_height(x, z) as i32 + dy).clamp(0, WORLD as i32 - 1) as u32;
                let m = physics::material_at(x, y, z);
                if m == M_LEAF || m == M_WOOD {
                    set_voxel(UVec3::new(x, y, z), M_FIRE);
                    add_burn_site(UVec3::new(x, y, z));
                    return;
                }
            }
        }
    }
}

// ── Ember integration ────────────────────────────────────────────

fn launch_ember(origin: Vec3) {
    let embers = unsafe { &mut *(&raw mut EMBERS) };
    for e in embers.iter_mut() {
        if e.active { continue; }
        let vx = rand_signed() * EMBER_VEL_XZ;
        let vz = rand_signed() * EMBER_VEL_XZ;
        let vy = EMBER_VEL_Y_MIN + rand_unit() * (EMBER_VEL_Y_MAX - EMBER_VEL_Y_MIN);
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
    // All slots busy — drop this ember silently.
}

/// Clear an ember's previously-painted voxel, but only if it's still
/// our ember marker (sometimes the CA or another rule has already
/// overwritten it).
fn clear_ember_voxel(p: UVec3) {
    if physics::material_at(p.x, p.y, p.z) == M_EMBER {
        set_voxel(p, 0);
    }
}

/// Walk each tracked burn site's 6 cardinal neighbours and add any
/// cell that's now `M_FIRE` (via §10.3 propagation) but not yet
/// tracked. Without this the cart-side sites would die as soon as the
/// original ignition burned out, even though the fire has actually
/// walked into adjacent cells.
fn discover_propagated_fire() {
    const NEIGHBOURS: [(i32, i32, i32); 6] = [
        (-1, 0, 0), (1, 0, 0),
        (0, -1, 0), (0, 1, 0),
        (0, 0, -1), (0, 0, 1),
    ];
    let sites = unsafe { &mut *(&raw mut BURN_SITES) };
    // Snapshot the currently-known positions so we don't pick up our
    // own additions in this pass.
    let mut known: [UVec3; BURN_SITES_CAP] = [UVec3 { x: 0, y: 0, z: 0 }; BURN_SITES_CAP];
    let mut known_count = 0usize;
    for slot in sites.iter() {
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
                    already = true; break;
                }
            }
            if !already {
                add_burn_site(UVec3::new(nx, ny, nz));
            }
        }
    }
}

/// One full tick of the ember system: discover newly-propagated fire,
/// roll each burn site for a possible new ember, then integrate every
/// airborne ember by one step.
pub(crate) fn tick() {
    discover_propagated_fire();

    // ── Phase 1: roll each burn site for a new ember launch ───
    //
    // A site is only allowed to launch when the cell it points at is
    // *still* M_FIRE. The §10.3 fire rule consumes each cell in ~12
    // ticks, but burn sites can stay tracked for much longer; we
    // don't want a long tail of embers spawning from a patch of grass
    // that *used to* be on fire.
    let sites = unsafe { &mut *(&raw mut BURN_SITES) };
    for slot in sites.iter_mut() {
        if let Some((pos, ttl)) = *slot {
            if physics::material_at(pos.x, pos.y, pos.z) != M_FIRE {
                *slot = None;
                continue;
            }
            if ttl == 0 { *slot = None; continue; }
            *slot = Some((pos, ttl - 1));
            if ember_rand() % SITE_LAUNCH_MOD != 0 { continue; }
            // Origin = exact burn site, lifted half a cell so the
            // very first paint doesn't fight the active fire voxel.
            let origin = Vec3::new(pos.x as f32 + 0.5, pos.y as f32 + 1.0, pos.z as f32 + 0.5);
            launch_ember(origin);
        }
    }

    // ── Phase 2: step every airborne ember ────────────────────
    let embers = unsafe { &mut *(&raw mut EMBERS) };
    let mut new_sites: [Option<UVec3>; EMBERS_CAP] = [None; EMBERS_CAP];
    let mut new_site_count = 0usize;

    for e in embers.iter_mut() {
        if !e.active { continue; }

        // Clear last-painted cell before stepping forward.
        if e.painted { clear_ember_voxel(e.last); e.painted = false; }

        if e.ttl == 0 { e.active = false; continue; }
        e.ttl -= 1;

        // Integrate position + velocity.
        e.pos = Vec3::new(e.pos.x + e.vel.x, e.pos.y + e.vel.y, e.pos.z + e.vel.z);
        e.vel = Vec3::new(e.vel.x, e.vel.y - EMBER_GRAVITY, e.vel.z);

        // Snap to integer cell. Clamp to world bounds; if we left
        // the world, drop.
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

        if m == M_LEAF || m == M_WOOD {
            // Ignition! Drop a fire voxel and birth a new burn site.
            set_voxel(cell, M_FIRE);
            if new_site_count < new_sites.len() {
                new_sites[new_site_count] = Some(cell);
                new_site_count += 1;
            }
            e.active = false;
            continue;
        }
        if m != 0 && m != M_EMBER {
            // Hit a non-flammable solid (terrain, water, etc.) —
            // snuff the ember.
            e.active = false;
            continue;
        }

        // Empty (or our own previous trail) — paint and continue.
        set_voxel(cell, M_EMBER);
        e.last = cell;
        e.painted = true;
    }

    for i in 0..new_site_count {
        if let Some(p) = new_sites[i] { add_burn_site(p); }
    }
}
