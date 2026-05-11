//! Cellular automata — Layer 3 (§10.3).
//!
//! Sparse active-set sim: only voxels actively doing something pay
//! anything. State is stored OUTSIDE the world voxel grid in a
//! `HashMap<u32 morton key, (insertion_tick, state_byte)>`. World
//! voxels stay 8-bit (§2).
//!
//! Drain order (deterministic, per §10.3): `(insertion_tick,
//! morton_position)`. `tick_counter` advances once per
//! [`CaState::tick`] call and is the per-frame insertion stamp.
//!
//! v0.1.5 scope: granular + level-aware liquid. The liquid rule reads
//! and writes `state_byte & 0x0F` as a fluid level 0..15 (§10.3 state
//! byte for `liquid`: "bits 0–3: fluid level"). Gas / flammable / fire
//! are reserved in [`MaterialFlags`] and the framework dispatches them,
//! but the rules are no-ops for now.

use std::collections::HashMap;

use voxlconsl_types::MaterialFlags;

use crate::world::{WorldState, WORLD_SIDE};

/// Per-port default budget, taken from §10.3's "browser, generous" cap.
pub const DEFAULT_BUDGET: u32 = 32_768;

/// Maximum liquid level stored in `state & 0x0F`.
pub const LIQUID_LEVEL_MAX: u8 = 15;

/// Default gas lifetime (in CA ticks) when a material's `ca_lifetime`
/// is 0. ~4 seconds at 60 fps; smoke typically dissipates in this
/// window.
pub const GAS_DEFAULT_LIFETIME: u8 = 255;

/// Default fire temperature stored in `state & 0x0F`. 15 is the spec
/// cap (4-bit temp).
pub const FIRE_DEFAULT_TEMP: u8 = 15;

/// Default fire remaining life stored in `(state >> 4) & 0x0F`.
/// 4-bit field, so 15 is the hard cap. Carts wanting longer fire
/// must restart cells.
pub const FIRE_DEFAULT_LIFE: u8 = 15;

/// Default flammable ignition threshold (heat units) when a material's
/// `ca_threshold` is 0. At `FIRE_DEFAULT_TEMP=15` heat/tick, 60 means a
/// cardinal flammable neighbor of one fire cell ignites in 4 ticks.
pub const FLAMMABLE_DEFAULT_THRESHOLD: u8 = 60;

#[inline]
fn pack_fire_state(temp: u8, life: u8) -> u8 {
    (temp & 0x0F) | ((life & 0x0F) << 4)
}

#[inline]
fn fire_state_temp(state: u8) -> u8 { state & 0x0F }

#[inline]
fn fire_state_life(state: u8) -> u8 { (state >> 4) & 0x0F }

/// Active-set entry: when it was inserted (for FIFO drain order) plus
/// the per-material 8-bit state byte.
#[derive(Copy, Clone, Debug, Default)]
pub struct ActiveEntry {
    pub insertion_tick: u32,
    pub state: u8,
}

pub struct CaState {
    /// Per-frame budget. 0 disables CA entirely.
    pub budget: u32,
    /// Active voxels keyed by Morton-encoded 3D position.
    active: HashMap<u32, ActiveEntry>,
    /// Monotonic tick counter, bumped at the start of each [`tick`].
    tick_counter: u32,
}

impl CaState {
    pub fn new() -> Self {
        Self {
            budget: DEFAULT_BUDGET,
            active: HashMap::new(),
            tick_counter: 0,
        }
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Insert/update an active-set entry for `(x, y, z)` and wake its
    /// 6-axis neighbors. Used by world mutations (cart writes,
    /// `fill_box`) where the caller has decided which CA state byte the
    /// new voxel should have — granular voxels use 0; freshly-placed
    /// liquids use 15 (full).
    ///
    /// Existing entries' state bytes are overwritten for the self
    /// position. Neighbors are inserted only when missing, with state
    /// 0 — the worker rule re-reads each neighbor's material from the
    /// dense buffer next tick so the default state is only ever used
    /// for "wake up and check" purposes.
    pub fn wake_with_state(&mut self, x: u32, y: u32, z: u32, self_state: u8) {
        let tick = self.tick_counter;
        self.put_one(x, y, z, tick, self_state, true);
        if x > 0              { self.put_one(x - 1, y, z, tick, 0, false); }
        if y > 0              { self.put_one(x, y - 1, z, tick, 0, false); }
        if z > 0              { self.put_one(x, y, z - 1, tick, 0, false); }
        if x + 1 < WORLD_SIDE { self.put_one(x + 1, y, z, tick, 0, false); }
        if y + 1 < WORLD_SIDE { self.put_one(x, y + 1, z, tick, 0, false); }
        if z + 1 < WORLD_SIDE { self.put_one(x, y, z + 1, tick, 0, false); }
    }

    /// Insert `(x, y, z)` plus its 6-axis neighbors with state=0.
    /// Wrapper for callers that don't care about level (granular,
    /// fill_box corners). Kept as the old name for call-site clarity.
    pub fn mark_active(&mut self, x: u32, y: u32, z: u32) {
        self.wake_with_state(x, y, z, 0);
    }

    /// Insert/update only the self cell at `(x, y, z)`, without waking
    /// neighbors. Used by the liquid rule when transferring level
    /// between adjacent cells: we already woke the neighbor by writing
    /// to it (or by direct mark), so a level-only update is enough.
    /// Existing state is overwritten.
    pub fn set_state(&mut self, x: u32, y: u32, z: u32, state: u8) {
        if x >= WORLD_SIDE || y >= WORLD_SIDE || z >= WORLD_SIDE { return; }
        let tick = self.tick_counter;
        self.put_one(x, y, z, tick, state, true);
    }

    /// Read the state byte for `(x, y, z)`. Returns None if the voxel
    /// is not in the active set.
    pub fn get_state(&self, x: u32, y: u32, z: u32) -> Option<u8> {
        self.active.get(&morton3(x, y, z)).map(|e| e.state)
    }

    /// Read fluid level for a liquid voxel. Returns `LIQUID_LEVEL_MAX`
    /// when the voxel is not in the active set — the convention is
    /// that a settled liquid is full (level 15) until something
    /// re-activates it. The renderer uses this on the hot path.
    pub fn liquid_level(&self, x: u32, y: u32, z: u32) -> u8 {
        self.active
            .get(&morton3(x, y, z))
            .map(|e| e.state & 0x0F)
            .unwrap_or(LIQUID_LEVEL_MAX)
    }

    fn put_one(&mut self, x: u32, y: u32, z: u32, tick: u32, state: u8, overwrite: bool) {
        if x >= WORLD_SIDE || y >= WORLD_SIDE || z >= WORLD_SIDE { return; }
        let key = morton3(x, y, z);
        if overwrite {
            // `and_modify` re-stamps `insertion_tick` so callers can
            // detect "this cell was touched mid-tick" by comparing the
            // snapshot's stamp to the current one (used by `tick()` to
            // skip entries that a rule mutated during the same drain,
            // e.g., gas rising into a cell whose snapshot entry hasn't
            // been processed yet).
            self.active
                .entry(key)
                .and_modify(|e| { e.state = state; e.insertion_tick = tick; })
                .or_insert(ActiveEntry { insertion_tick: tick, state });
        } else {
            self.active
                .entry(key)
                .or_insert(ActiveEntry { insertion_tick: tick, state });
        }
    }

    fn evict(&mut self, x: u32, y: u32, z: u32) {
        self.active.remove(&morton3(x, y, z));
    }
}

// ============================================================================
// Per-frame drain.
// ============================================================================

/// Tick the active set: drain up to `budget` entries in
/// `(insertion_tick, morton_position)` order, dispatching each to the
/// rule for its material's flags.
pub fn tick(world: &mut WorldState) {
    if world.ca.budget == 0 { return; }
    world.ca.tick_counter = world.ca.tick_counter.wrapping_add(1);

    // Snapshot + sort. Spec §10.3: ordered by (insertion_tick,
    // morton_position). Snapshot is required because rule application
    // mutates the active set.
    let budget = world.ca.budget as usize;
    let mut entries: Vec<(u32, u32)> = world
        .ca
        .active
        .iter()
        .map(|(&k, e)| (e.insertion_tick, k))
        .collect();
    entries.sort_unstable_by_key(|&(t, k)| (t, k));
    if entries.len() > budget {
        entries.truncate(budget);
    }

    for (snapshot_tick, key) in entries {
        // Skip if a rule earlier in this tick re-stamped the cell —
        // means it was just placed or had its state rewritten and
        // shouldn't double-process. The classic case: gas rises into
        // (x, y+1, z); the snapshot still has an old "neighbor wake"
        // entry for (x, y+1, z), and without this guard the rule
        // would rise again the same tick, moving 2 cells.
        if let Some(e) = world.ca.active.get(&key) {
            if e.insertion_tick > snapshot_tick {
                continue;
            }
        }

        let (x, y, z) = unmorton3(key);
        // Voxel may have been overwritten by an earlier rule call this
        // frame — read fresh from the dense buffer.
        let m = world.read_material(x, y, z);
        if m == 0 {
            // Air — nothing to simulate. Evict and let neighbors handle
            // their own settling.
            world.ca.evict(x, y, z);
            continue;
        }
        let flags = world.materials[m as usize].flags;
        // Priority order: fire and gas take precedence over liquid and
        // granular when a material has both flags set (e.g., oil =
        // liquid+flammable runs liquid; a material flagged
        // fire+granular would run fire). This matters for the spec
        // example oil (§2) which combines liquid+flammable — for v0.1.6
        // oil runs as a regular liquid; ignition support requires a
        // future combined-rule pass.
        if flags.contains(MaterialFlags::FIRE) {
            fire_tick(world, x, y, z, m);
        } else if flags.contains(MaterialFlags::GAS) {
            gas_tick(world, x, y, z, m);
        } else if flags.contains(MaterialFlags::GRANULAR) {
            granular_tick(world, x, y, z, m);
        } else if flags.contains(MaterialFlags::LIQUID) {
            liquid_tick(world, x, y, z, m);
        } else if flags.contains(MaterialFlags::FLAMMABLE) {
            flammable_tick(world, x, y, z, m);
        } else {
            // No CA flags at all — material was replaced by something
            // inert. Evict.
            world.ca.evict(x, y, z);
        }
    }
}

/// Granular rule (sand-like): fall straight down if the cell below is
/// air; otherwise try diagonal slides; otherwise settle (evict).
fn granular_tick(world: &mut WorldState, x: u32, y: u32, z: u32, m: u8) {
    if y == 0 {
        world.ca.evict(x, y, z);
        return;
    }
    let below = world.read_material(x, y - 1, z);
    if below == 0 {
        // Fall straight down. set_voxel re-marks both old and new
        // positions automatically since the granular flag matches.
        world.set_voxel(x, y, z, 0);
        world.set_voxel(x, y - 1, z, m);
        return;
    }

    // Try the four cardinal diagonal slides in a fixed order so the
    // simulation is deterministic. A diagonal is only valid when both
    // the side cell at (nx, y, nz) AND the cell below it (nx, y-1, nz)
    // are air — otherwise the grain would clip through a solid
    // neighbor.
    const DIRS: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
    for &(dx, dz) in &DIRS {
        let nx = x as i32 + dx;
        let nz = z as i32 + dz;
        if nx < 0 || nx >= WORLD_SIDE as i32 { continue; }
        if nz < 0 || nz >= WORLD_SIDE as i32 { continue; }
        let nx = nx as u32;
        let nz = nz as u32;
        let side = world.read_material(nx, y, nz);
        let diag_below = world.read_material(nx, y - 1, nz);
        if side == 0 && diag_below == 0 {
            world.set_voxel(x, y, z, 0);
            world.set_voxel(nx, y - 1, nz, m);
            return;
        }
    }
    // Settled. Evict so we don't keep retrying every frame.
    world.ca.evict(x, y, z);
}

/// Liquid rule (water-like): level-aware mass-conserving flow.
///
/// Each liquid voxel carries a fluid level 0..15 in `state & 0x0F`
/// (§10.3 state byte for `liquid`). A freshly-placed source voxel
/// starts at 15 (full); the rule equilibrates levels with neighbors
/// until the body is at rest. Per tick:
///
///   1. Gravity. Donate as much as possible downward. If below is air,
///      the entire level falls straight down (the cell becomes air,
///      level 15 lands one Y below). If below is the same liquid with
///      partial fill, donate `min(self, 15 - below)` down.
///   2. Lateral equilibration. For each cardinal neighbor in fixed
///      order, if `self > neighbor + 1` (i.e., a slope of at least
///      two), donate one unit. Self stops donating once it drops to
///      level 1 (which is the smallest standing puddle).
///
/// A voxel that didn't transfer anything *and* has no pressure from
/// above evicts itself; otherwise it stays active for the next tick.
/// This produces flat-pond equilibria under a continuous source and
/// avoids the v0.1.3 single-voxel-pyramid limitation.
fn liquid_tick(world: &mut WorldState, x: u32, y: u32, z: u32, m: u8) {
    // Read our level. A liquid voxel that's already in the active set
    // with state=0 is *uninitialized* (woke up via a neighbor's 6-axis
    // wake which uses state=0 as a default) — not "empty". A real
    // "empty → clear" transition is handled inline below by writing
    // air + evicting, never by storing state=0 persistently.
    let raw = world.ca.get_state(x, y, z).unwrap_or(LIQUID_LEVEL_MAX);
    let mut level = raw & 0x0F;
    if level == 0 { level = LIQUID_LEVEL_MAX; }

    let mut transferred = false;

    // Step 1: gravity.
    if y > 0 {
        let bm = world.read_material(x, y - 1, z);
        if bm == 0 {
            // Free fall: move the entire level one cell down.
            world.set_voxel(x, y, z, 0);
            world.ca.evict(x, y, z);
            place_liquid(world, x, y - 1, z, m, level);
            return;
        }
        if bm == m {
            let bl = world.ca.liquid_level(x, y - 1, z);
            if bl < LIQUID_LEVEL_MAX {
                let space = LIQUID_LEVEL_MAX - bl;
                let mut send = level.min(space);
                // Don't drain to 0 unless we're being pressured by
                // liquid above. A "top-of-column" partial cell that
                // fully drained would just be re-created by a lateral
                // neighbor next tick, only to drain again — that's
                // the rim-flicker the user reports. Leaving 1 unit
                // behind keeps the cell stable while mass keeps
                // routing down via lateral donation cycles.
                let pressured_above = y + 1 < WORLD_SIDE
                    && world.read_material(x, y + 1, z) == m;
                if !pressured_above && send >= level {
                    send = level - 1;
                }
                if send > 0 {
                    world.ca.set_state(x, y - 1, z, bl + send);
                    level -= send;
                    transferred = true;
                    if level == 0 {
                        world.set_voxel(x, y, z, 0);
                        world.ca.evict(x, y, z);
                        return;
                    }
                }
            }
        }
    }

    // Step 2: lateral equilibration.
    if level >= 2 {
        const DIRS: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
        for &(dx, dz) in &DIRS {
            if level < 2 { break; }
            let nx = x as i32 + dx;
            let nz = z as i32 + dz;
            if nx < 0 || nx >= WORLD_SIDE as i32 { continue; }
            if nz < 0 || nz >= WORLD_SIDE as i32 { continue; }
            let nx = nx as u32;
            let nz = nz as u32;
            let nm = world.read_material(nx, y, nz);
            // Only spread into air or our own liquid.
            if nm != 0 && nm != m { continue; }

            // Diagonal-flow shortcut: if the lateral target is air AND
            // the cell below it is partial-fill same liquid, donate
            // directly into the lower cell. Skipping the intermediate
            // same-y cell prevents the "land at y, drain to y-1 next
            // tick, repeat" pop cycle that shows as voxel flicker at
            // the pond rim.
            if nm == 0 && y > 0 {
                let nm_below = world.read_material(nx, y - 1, nz);
                if nm_below == m {
                    let bl = world.ca.liquid_level(nx, y - 1, nz);
                    if bl < LIQUID_LEVEL_MAX {
                        world.ca.set_state(nx, y - 1, nz, bl + 1);
                        level -= 1;
                        transferred = true;
                        continue;
                    }
                }
            }

            let nl = if nm == m {
                world.ca.liquid_level(nx, y, nz)
            } else {
                0
            };
            // Only donate when there's a slope of at least two — this
            // is what stops 1-unit voxels from flickering back and
            // forth across cardinals.
            if level > nl + 1 {
                let new_nl = nl + 1;
                if nm == 0 {
                    place_liquid(world, nx, y, nz, m, new_nl);
                } else {
                    // Existing same-liquid neighbor: bump its level in
                    // place. set_state already puts/updates its
                    // active-set entry; we deliberately do NOT call
                    // mark_active here, which would overwrite the
                    // state with 0 via wake_with_state.
                    world.ca.set_state(nx, y, nz, new_nl);
                }
                level -= 1;
                transferred = true;
            }
        }
    }

    // Write our remaining level back.
    if level == 0 {
        world.set_voxel(x, y, z, 0);
        world.ca.evict(x, y, z);
        return;
    }
    world.ca.set_state(x, y, z, level);

    // Stay active if we did anything this tick OR if we're still being
    // pressured by liquid from above OR if we're holding a partial
    // level (the level lives in the active-set state byte, so evicting
    // a level-<15 cell would lose the data — the renderer would see it
    // as full again next frame).
    //
    // A settled full cell (L=15 with no transfers and no pressure) is
    // safe to evict: a future lookup defaults to LIQUID_LEVEL_MAX, so
    // the renderer still sees it as full.
    let pressured_above = y + 1 < WORLD_SIDE
        && world.read_material(x, y + 1, z) == m;
    if !transferred && !pressured_above && level == LIQUID_LEVEL_MAX {
        world.ca.evict(x, y, z);
    }
}

/// Place a liquid voxel at `(x, y, z)` and set its CA level. Used both
/// for free-fall (move entire level into an air cell) and lateral
/// spread (donate one unit into an air cell, creating a new level=1
/// voxel). `set_voxel`'s default LIQUID-init runs first; we then
/// override with the caller's level.
fn place_liquid(world: &mut WorldState, x: u32, y: u32, z: u32, m: u8, level: u8) {
    world.set_voxel(x, y, z, m);
    world.ca.set_state(x, y, z, level);
}

// ============================================================================
// Gas rule (smoke, steam, ...).
//
// State byte = lifetime countdown 0..255 (§10.3 "bits 0–7: lifetime
// countdown"). A gas voxel tries to rise into the cell above each
// tick; if blocked, it tries to spread laterally in a fixed
// (-X, +X, -Z, +Z) order; if everything is blocked it stays put.
// Either way, lifetime decrements by 1, and on reaching 0 the cell
// vanishes to air.
//
// Source-init (cart-driven `set_voxel`) writes the configured
// `ca_lifetime` (or `GAS_DEFAULT_LIFETIME` when 0). The CA rule's own
// placements via `place_gas` carry the remaining lifetime forward.
// ============================================================================

fn gas_tick(world: &mut WorldState, x: u32, y: u32, z: u32, m: u8) {
    let lifetime = world.ca.get_state(x, y, z).unwrap_or(GAS_DEFAULT_LIFETIME);
    if lifetime == 0 {
        world.set_voxel(x, y, z, 0);
        world.ca.evict(x, y, z);
        return;
    }
    let new_lifetime = lifetime - 1;

    // Try to rise into air above.
    if y + 1 < WORLD_SIDE && world.read_material(x, y + 1, z) == 0 {
        world.set_voxel(x, y, z, 0);
        world.ca.evict(x, y, z);
        place_gas(world, x, y + 1, z, m, new_lifetime);
        return;
    }

    // Else try lateral spread, fixed cardinal order.
    const DIRS: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
    for &(dx, dz) in &DIRS {
        let nx = x as i32 + dx;
        let nz = z as i32 + dz;
        if nx < 0 || nx >= WORLD_SIDE as i32 { continue; }
        if nz < 0 || nz >= WORLD_SIDE as i32 { continue; }
        let nx = nx as u32;
        let nz = nz as u32;
        if world.read_material(nx, y, nz) == 0 {
            world.set_voxel(x, y, z, 0);
            world.ca.evict(x, y, z);
            place_gas(world, nx, y, nz, m, new_lifetime);
            return;
        }
    }

    // Boxed in — stay, but keep aging.
    world.ca.set_state(x, y, z, new_lifetime);
}

fn place_gas(world: &mut WorldState, x: u32, y: u32, z: u32, m: u8, lifetime: u8) {
    world.set_voxel(x, y, z, m);
    world.ca.set_state(x, y, z, lifetime);
}

// ============================================================================
// Fire rule.
//
// State byte packs `(temp & 0x0F) | ((life & 0x0F) << 4)` (§10.3
// "bits 0–3: temperature; bits 4–7: remaining life"). Each tick the
// cell decrements its life; at 0 it vanishes to air. Temperature is
// constant for the cell's lifetime — the flammable rule reads it
// when computing per-tick heat contribution from this cell.
//
// Source-init writes `(FIRE_DEFAULT_TEMP, ca_lifetime.min(15))` (or
// `FIRE_DEFAULT_LIFE` when the cart sets `ca_lifetime` to 0). 4-bit
// life caps at 15 ticks; carts wanting longer fire must re-light
// cells from their own logic (or use embers — that's a cart-level
// trick).
// ============================================================================

fn fire_tick(world: &mut WorldState, x: u32, y: u32, z: u32, _m: u8) {
    let default = pack_fire_state(FIRE_DEFAULT_TEMP, FIRE_DEFAULT_LIFE);
    let raw = world.ca.get_state(x, y, z).unwrap_or(default);
    let temp = fire_state_temp(raw);
    let life = fire_state_life(raw);
    if life == 0 {
        world.set_voxel(x, y, z, 0);
        world.ca.evict(x, y, z);
        return;
    }
    world.ca.set_state(x, y, z, pack_fire_state(temp, life - 1));
}

// ============================================================================
// Flammable rule.
//
// State byte = accumulated heat 0..255 (§10.3 "bits 0–7: accumulated
// heat"). Each tick the cell scans its 6 cardinal neighbors for any
// FIRE-flagged material and adds each fire neighbor's temperature to
// its own heat. When heat ≥ threshold the cell ignites: it becomes
// `material.ignites_to` (0 = vanish to air; otherwise typically the
// cart's fire material). Cells with no fire neighbors evict — heat
// state is not preserved across "fire moves away" / "fire returns".
// ============================================================================

fn flammable_tick(world: &mut WorldState, x: u32, y: u32, z: u32, m: u8) {
    let heat = world.ca.get_state(x, y, z).unwrap_or(0);

    let mut new_heat = heat;
    let mut found_fire = false;
    const NEIGHBORS: [(i32, i32, i32); 6] = [
        (-1, 0, 0), (1, 0, 0),
        (0, -1, 0), (0, 1, 0),
        (0, 0, -1), (0, 0, 1),
    ];
    let default_fire_state = pack_fire_state(FIRE_DEFAULT_TEMP, FIRE_DEFAULT_LIFE);
    for &(dx, dy, dz) in &NEIGHBORS {
        let nx = x as i32 + dx;
        let ny = y as i32 + dy;
        let nz = z as i32 + dz;
        if nx < 0 || nx >= WORLD_SIDE as i32 { continue; }
        if ny < 0 || ny >= WORLD_SIDE as i32 { continue; }
        if nz < 0 || nz >= WORLD_SIDE as i32 { continue; }
        let nx = nx as u32;
        let ny = ny as u32;
        let nz = nz as u32;
        let nm = world.read_material(nx, ny, nz);
        if nm == 0 { continue; }
        if world.materials[nm as usize].flags.contains(MaterialFlags::FIRE) {
            let fire_state = world.ca.get_state(nx, ny, nz).unwrap_or(default_fire_state);
            new_heat = new_heat.saturating_add(fire_state_temp(fire_state));
            found_fire = true;
        }
    }

    let threshold = {
        let raw = world.materials[m as usize].ca_threshold;
        if raw == 0 { FLAMMABLE_DEFAULT_THRESHOLD } else { raw }
    };

    if new_heat >= threshold {
        let ignites_to = world.materials[m as usize].ignites_to;
        if ignites_to == 0 {
            world.set_voxel(x, y, z, 0);
        } else {
            // set_voxel's CA hook auto-inits fire state when the new
            // material has the FIRE flag.
            world.set_voxel(x, y, z, ignites_to);
        }
        // set_voxel above already evicted/replaced our entry as
        // needed; for the air-vanish path, evict to be explicit.
        if ignites_to == 0 {
            world.ca.evict(x, y, z);
        }
        return;
    }

    if found_fire {
        world.ca.set_state(x, y, z, new_heat);
    } else {
        // No fire nearby — no preheating, drop state.
        world.ca.evict(x, y, z);
    }
}

// ============================================================================
// Morton-3 encode/decode for 9-bit-per-axis world coords.
// ============================================================================

#[inline]
pub fn morton3(x: u32, y: u32, z: u32) -> u32 {
    spread3(x & 0x1FF) | (spread3(y & 0x1FF) << 1) | (spread3(z & 0x1FF) << 2)
}

fn spread3(v: u32) -> u32 {
    // Interleave a 10-bit value's bits over 30 bits (one bit, then two
    // zero bits, then next bit, etc.).
    let mut v = v & 0x3FF;
    v = (v | (v << 16)) & 0x030000FF;
    v = (v | (v <<  8)) & 0x0300F00F;
    v = (v | (v <<  4)) & 0x030C30C3;
    v = (v | (v <<  2)) & 0x09249249;
    v
}

#[inline]
pub fn unmorton3(m: u32) -> (u32, u32, u32) {
    (compact3(m), compact3(m >> 1), compact3(m >> 2))
}

fn compact3(v: u32) -> u32 {
    let mut v = v & 0x09249249;
    v = (v | (v >>  2)) & 0x030C30C3;
    v = (v | (v >>  4)) & 0x0300F00F;
    v = (v | (v >>  8)) & 0x030000FF;
    v = (v | (v >> 16)) & 0x000003FF;
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxlconsl_types::{Material, MaterialFlags};

    fn world_with_sand_at(x: u32, y: u32, z: u32) -> WorldState {
        let mut world = WorldState::new();
        // Material 1 = sand (granular).
        world.materials[1] = Material {
            flags: MaterialFlags::empty().with(MaterialFlags::GRANULAR),
            ..Material::default()
        };
        world.set_voxel(x, y, z, 1);
        world
    }

    #[test]
    fn morton_roundtrip() {
        for &(x, y, z) in &[(0, 0, 0), (1, 0, 0), (260, 14, 260), (511, 511, 511)] {
            let m = morton3(x, y, z);
            assert_eq!(unmorton3(m), (x, y, z));
        }
    }

    #[test]
    fn granular_falls_straight_down() {
        let mut world = world_with_sand_at(10, 5, 10);
        // The set_voxel hooks below register active for sand voxel.
        // Tick once: sand should drop from y=5 to y=4.
        tick(&mut world);
        assert_eq!(world.read_material(10, 5, 10), 0, "old slot should be air");
        assert_eq!(world.read_material(10, 4, 10), 1, "sand should be one below");
    }

    #[test]
    fn granular_settles_on_floor() {
        // Place sand directly on y=1 over a stone block at y=0.
        let mut world = world_with_sand_at(10, 1, 10);
        world.materials[2] = Material::default(); // stone, no CA flags
        world.set_voxel(10, 0, 10, 2); // stone below
        tick(&mut world);
        let still_there = world.read_material(10, 1, 10);
        if still_there == 1 {
            // Settled in place — OK.
        } else {
            // Confirm sand landed in some adjacent column at y=0.
            let mut found = false;
            for &(dx, dz) in &[(-1i32, 0), (1, 0), (0, -1), (0, 1)] {
                let nx = (10i32 + dx) as u32;
                let nz = (10i32 + dz) as u32;
                if world.read_material(nx, 0, nz) == 1 {
                    found = true;
                    break;
                }
            }
            assert!(found, "sand slid off but isn't visible in any neighbor");
        }
    }

    fn flat_stone_5x5() -> WorldState {
        let mut world = WorldState::new();
        world.materials[1] = Material {
            flags: MaterialFlags::empty().with(MaterialFlags::LIQUID),
            ..Material::default()
        };
        world.materials[2] = Material::default();
        for dz in -2i32..=2 {
            for dx in -2..=2 {
                let nx = (10i32 + dx) as u32;
                let nz = (10i32 + dz) as u32;
                world.set_voxel(nx, 0, nz, 2);
            }
        }
        world
    }

    /// Sum up `level` across every cell whose material is `liquid`. For
    /// cells not in the active set, level is treated as 15 (matching
    /// `liquid_level`'s default).
    fn total_liquid_mass(world: &WorldState, liquid_mat: u8) -> u32 {
        let mut sum = 0u32;
        for z in 0..WORLD_SIDE {
            for y in 0..WORLD_SIDE.min(8) {
                for x in 0..WORLD_SIDE {
                    if world.read_material(x, y, z) == liquid_mat {
                        sum += world.ca.liquid_level(x, y, z) as u32;
                    }
                }
            }
        }
        sum
    }

    #[test]
    fn liquid_source_initialized_to_full_level() {
        // A freshly placed liquid voxel must enter the active set at
        // level 15, not level 0 (which would mean "empty / clear me").
        let mut world = flat_stone_5x5();
        world.set_voxel(10, 1, 10, 1);
        assert_eq!(world.ca.liquid_level(10, 1, 10), 15);
    }

    #[test]
    fn liquid_falls_into_air() {
        // Liquid voxel suspended in air should fall straight down by
        // one cell per tick.
        let mut world = flat_stone_5x5();
        // Floor at y=0, then air rows y=1..3, then a water voxel at y=3.
        world.set_voxel(10, 3, 10, 1);
        tick(&mut world);
        assert_eq!(world.read_material(10, 3, 10), 0);
        assert_eq!(world.read_material(10, 2, 10), 1);
        assert_eq!(world.ca.liquid_level(10, 2, 10), 15);
    }

    #[test]
    fn liquid_spreads_laterally_when_pressured_from_above() {
        // Stack two water voxels on the floor; the bottom one is full
        // (level 15) and pressured by another full voxel above. Spread
        // outward over enough ticks for the level-1 ring to appear.
        let mut world = flat_stone_5x5();
        world.set_voxel(10, 1, 10, 1);
        world.set_voxel(10, 2, 10, 1);
        for _ in 0..16 { tick(&mut world); }
        let mut neighbor_water = 0;
        for &(dx, dz) in &[(-1i32, 0), (1, 0), (0, -1), (0, 1)] {
            let nx = (10i32 + dx) as u32;
            let nz = (10i32 + dz) as u32;
            if world.read_material(nx, 1, nz) == 1 {
                neighbor_water += 1;
            }
        }
        assert!(neighbor_water > 0,
            "pressured water didn't spread to any neighbor");
    }

    #[test]
    fn liquid_stream_lays_flat_not_pyramidal() {
        // A continuous source of falling water voxels should spread out
        // along the ground rather than stacking. After enough time
        // there should be more water voxels at y=1 (the ground row)
        // than at y=2.
        let mut world = WorldState::new();
        world.materials[1] = Material {
            flags: MaterialFlags::empty().with(MaterialFlags::LIQUID),
            ..Material::default()
        };
        world.materials[2] = Material::default();
        for dz in -10i32..=10 {
            for dx in -10..=10 {
                let nx = (10i32 + dx) as u32;
                let nz = (10i32 + dz) as u32;
                world.set_voxel(nx, 0, nz, 2);
            }
        }
        // Drop 60 water voxels into the same column.
        for _ in 0..60 {
            if world.read_material(10, 8, 10) == 0 {
                world.set_voxel(10, 8, 10, 1);
            }
            tick(&mut world);
        }
        // Let in-flight voxels land + equilibrate.
        for _ in 0..200 { tick(&mut world); }

        let mut at_y1 = 0;
        let mut at_y2 = 0;
        for dz in -10i32..=10 {
            for dx in -10..=10 {
                let nx = (10i32 + dx) as u32;
                let nz = (10i32 + dz) as u32;
                if world.read_material(nx, 1, nz) == 1 { at_y1 += 1; }
                if world.read_material(nx, 2, nz) == 1 { at_y2 += 1; }
            }
        }
        assert!(at_y1 >= 8,
            "expected a wide bottom row of water; only {} voxels at y=1", at_y1);
        assert!(at_y1 > at_y2,
            "water piled vertically (y=1: {}, y=2: {}); expected y=1 > y=2",
            at_y1, at_y2);
    }

    #[test]
    fn liquid_mass_is_conserved_under_spread() {
        // Place one full water voxel on a flat floor; tick to settle.
        // The total fluid level summed across all liquid cells should
        // equal 15 forever (mass conservation).
        let mut world = flat_stone_5x5();
        // Widen the floor so the puddle has room.
        for dz in -10i32..=10 {
            for dx in -10..=10 {
                let nx = (10i32 + dx) as u32;
                let nz = (10i32 + dz) as u32;
                world.set_voxel(nx, 0, nz, 2);
            }
        }
        world.set_voxel(10, 1, 10, 1);
        let start_mass = total_liquid_mass(&world, 1);
        assert_eq!(start_mass, 15);
        for _ in 0..200 { tick(&mut world); }
        let end_mass = total_liquid_mass(&world, 1);
        assert_eq!(end_mass, 15, "mass not conserved under lateral spread");
    }

    #[test]
    fn liquid_level_below_15_advances_top_surface_marker() {
        // Smoke-test the renderer-visible level state: write a partial
        // level directly and confirm `liquid_level` returns it.
        let mut world = flat_stone_5x5();
        world.set_voxel(10, 1, 10, 1);
        world.ca.set_state(10, 1, 10, 7);
        assert_eq!(world.ca.liquid_level(10, 1, 10), 7);
    }

    #[test]
    fn liquid_lone_voxel_does_not_flicker() {
        // A single voxel placed on solid ground spreads out as far as
        // it can (per equilibration), then stops moving. Critically,
        // it must not oscillate between cells.
        let mut world = flat_stone_5x5();
        // Widen so the equilibrium puddle has room.
        for dz in -3i32..=3 {
            for dx in -3..=3 {
                let nx = (10i32 + dx) as u32;
                let nz = (10i32 + dz) as u32;
                world.set_voxel(nx, 0, nz, 2);
            }
        }
        world.set_voxel(10, 1, 10, 1);
        // Run long enough for the puddle to settle.
        for _ in 0..200 { tick(&mut world); }
        // Run another batch and snapshot positions — they must not
        // change after settling.
        let snapshot: Vec<(u32, u32, u32, u8)> = {
            let mut v = Vec::new();
            for dz in -3i32..=3 {
                for dx in -3..=3 {
                    let nx = (10i32 + dx) as u32;
                    let nz = (10i32 + dz) as u32;
                    if world.read_material(nx, 1, nz) == 1 {
                        v.push((nx, 1, nz, world.ca.liquid_level(nx, 1, nz)));
                    }
                }
            }
            v
        };
        for _ in 0..16 { tick(&mut world); }
        let after: Vec<(u32, u32, u32, u8)> = {
            let mut v = Vec::new();
            for dz in -3i32..=3 {
                for dx in -3..=3 {
                    let nx = (10i32 + dx) as u32;
                    let nz = (10i32 + dz) as u32;
                    if world.read_material(nx, 1, nz) == 1 {
                        v.push((nx, 1, nz, world.ca.liquid_level(nx, 1, nz)));
                    }
                }
            }
            v
        };
        assert_eq!(snapshot, after, "puddle moved after settling — flicker");
    }

    #[test]
    fn granular_evicts_when_no_move() {
        // Build a "cup" — sand surrounded by stone on all sides, with
        // stone below as floor. Only place to go is up (impossible).
        let mut world = WorldState::new();
        world.materials[1] = Material {
            flags: MaterialFlags::empty().with(MaterialFlags::GRANULAR),
            ..Material::default()
        };
        world.materials[2] = Material::default();
        // Floor + walls surrounding (10, 1, 10) at y=0..=1
        for &(dx, dz) in &[(-1i32, 0), (1, 0), (0, -1), (0, 1), (0, 0)] {
            let nx = (10i32 + dx) as u32;
            let nz = (10i32 + dz) as u32;
            world.set_voxel(nx, 0, nz, 2);
        }
        for &(dx, dz) in &[(-1i32, 0), (1, 0), (0, -1), (0, 1)] {
            let nx = (10i32 + dx) as u32;
            let nz = (10i32 + dz) as u32;
            world.set_voxel(nx, 1, nz, 2);
        }
        // Place sand at (10, 1, 10): stone walls all around at y=1.
        world.set_voxel(10, 1, 10, 1);

        let active_before = world.ca.active_count();
        tick(&mut world);
        let active_after = world.ca.active_count();
        // Sand should still be at (10, 1, 10) and the entry was evicted.
        assert_eq!(world.read_material(10, 1, 10), 1);
        assert!(active_after < active_before,
            "expected at least the sand voxel to be evicted (was {}, now {})",
            active_before, active_after);
    }

    // ====================================================================
    // Gas / fire / flammable.
    // ====================================================================

    /// World with: slot 1 = gas, slot 2 = inert solid, slot 3 = fire
    /// (emissive), slot 4 = flammable that ignites to slot 3.
    fn world_with_gas_fire_flammable() -> WorldState {
        let mut world = WorldState::new();
        world.materials[1] = Material {
            flags: MaterialFlags::empty().with(MaterialFlags::GAS),
            ..Material::default()
        };
        world.materials[2] = Material::default();
        world.materials[3] = Material {
            flags: MaterialFlags::empty().with(MaterialFlags::FIRE),
            ..Material::default()
        };
        world.materials[4] = Material {
            flags: MaterialFlags::empty().with(MaterialFlags::FLAMMABLE),
            ignites_to: 3,
            ..Material::default()
        };
        world
    }

    #[test]
    fn gas_rises_into_air_above() {
        let mut world = world_with_gas_fire_flammable();
        world.set_voxel(10, 4, 10, 1);
        tick(&mut world);
        assert_eq!(world.read_material(10, 4, 10), 0, "gas should leave start cell");
        assert_eq!(world.read_material(10, 5, 10), 1, "gas should be one cell higher");
    }

    #[test]
    fn gas_spreads_laterally_when_blocked_above() {
        let mut world = world_with_gas_fire_flammable();
        // Ceiling of solid at y=5, gas at y=4 with all 4 cardinals at y=4
        // free. After one tick the gas should have moved to one of its
        // cardinal neighbors.
        world.set_voxel(10, 5, 10, 2);
        world.set_voxel(10, 4, 10, 1);
        tick(&mut world);
        let cardinals = [(-1i32, 0), (1, 0), (0, -1), (0, 1)];
        let mut found = false;
        for (dx, dz) in cardinals {
            let nx = (10i32 + dx) as u32;
            let nz = (10i32 + dz) as u32;
            if world.read_material(nx, 4, nz) == 1 {
                found = true;
                break;
            }
        }
        assert!(found, "gas should spread to a cardinal at same y when ceiling blocks rising");
    }

    #[test]
    fn gas_decays_to_air_after_lifetime() {
        let mut world = world_with_gas_fire_flammable();
        // Configure the gas to have a short lifetime so the test is fast.
        world.materials[1].ca_lifetime = 3;
        // Trap the gas in a 1×1 air column so it can't rise away — easier
        // to assert on the lifetime path.
        world.set_voxel(10, 5, 10, 2); // ceiling
        world.set_voxel( 9, 4, 10, 2); world.set_voxel(11, 4, 10, 2);
        world.set_voxel(10, 4,  9, 2); world.set_voxel(10, 4, 11, 2);
        world.set_voxel(10, 3, 10, 2); // floor (gas can't go down anyway, but blocks below)
        world.set_voxel(10, 4, 10, 1);
        // 3 ticks should drain the lifetime to 0; one more tick clears.
        for _ in 0..5 { tick(&mut world); }
        assert_eq!(world.read_material(10, 4, 10), 0, "gas should have decayed to air");
    }

    #[test]
    fn fire_decays_to_air_after_life() {
        let mut world = world_with_gas_fire_flammable();
        world.materials[3].ca_lifetime = 4; // 4-frame fire
        world.set_voxel(10, 1, 10, 3);
        // life=4 → 4 ticks decrement to 0, next tick clears.
        for _ in 0..6 { tick(&mut world); }
        assert_eq!(world.read_material(10, 1, 10), 0, "fire should have burned out");
    }

    #[test]
    fn flammable_ignites_into_configured_fire_material() {
        let mut world = world_with_gas_fire_flammable();
        // Low threshold so ignition is fast under FIRE_DEFAULT_TEMP=15.
        world.materials[4].ca_threshold = 15;
        // Long fire life so it doesn't burn out before we observe ignition.
        world.materials[3].ca_lifetime = 15;
        world.set_voxel(10, 1, 10, 4); // flammable
        world.set_voxel(11, 1, 10, 3); // adjacent fire
        // First tick: flammable accumulates 15 heat from fire neighbor →
        // meets threshold → ignites to fire.
        tick(&mut world);
        assert_eq!(world.read_material(10, 1, 10), 3,
            "flammable should have ignited into the configured fire material");
    }

    #[test]
    fn flammable_without_ignites_to_burns_to_air() {
        let mut world = world_with_gas_fire_flammable();
        world.materials[4].ignites_to = 0; // ignite-to-air
        world.materials[4].ca_threshold = 15;
        world.materials[3].ca_lifetime = 15;
        world.set_voxel(10, 1, 10, 4);
        world.set_voxel(11, 1, 10, 3);
        tick(&mut world);
        assert_eq!(world.read_material(10, 1, 10), 0,
            "flammable with ignites_to=0 should vanish on ignition");
    }

    #[test]
    fn fire_propagates_along_a_row_of_flammables() {
        let mut world = world_with_gas_fire_flammable();
        world.materials[4].ca_threshold = 15; // 1-tick ignition
        world.materials[3].ca_lifetime = 15;  // long enough to cascade
        // Lay 5 flammable cells in a row, plus one fire at the left end.
        for dx in 0..5 {
            world.set_voxel(11 + dx, 1, 10, 4);
        }
        world.set_voxel(10, 1, 10, 3);
        // Run enough ticks for the wavefront to traverse the row.
        for _ in 0..20 { tick(&mut world); }
        // Each cell should have transitioned through fire at some point.
        // Since fire burns out after 15 ticks (cumulative), some cells may
        // already be air. At minimum, NONE of the original flammables
        // should still read as flammable (material 4).
        for dx in 0..5 {
            let m = world.read_material(11 + dx, 1, 10);
            assert_ne!(m, 4,
                "flammable at x={} never ignited (material still {})", 11 + dx, m);
        }
    }

    #[test]
    fn lone_flammable_without_fire_neighbor_does_not_ignite() {
        let mut world = world_with_gas_fire_flammable();
        world.materials[4].ca_threshold = 1; // would ignite from any heat
        world.set_voxel(10, 1, 10, 4);
        for _ in 0..50 { tick(&mut world); }
        assert_eq!(world.read_material(10, 1, 10), 4,
            "lone flammable with no fire nearby should stay put");
    }
}
