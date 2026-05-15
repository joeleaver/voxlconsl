//! Fire engine — road-locked vehicle that snuffs nearby fire from a
//! parked position. Distinct from the firetruck (the crew vehicle
//! that lays firebreaks): the engine can ONLY drive on road cells,
//! so its dispatch is restricted to road-anchored deploys. Once
//! parked it runs its hose continuously, clearing M_FIRE / M_EMBER
//! cells inside `HOSE_RADIUS`. When no fire remains in range for
//! `NO_FIRE_TIMEOUT` ticks the engine drives itself back to base.
//!
//! ## Pathfinding
//!
//! The cart's road network is a horizontal 3-wide strip at z =
//! TOWN_Z (currently x = 100..210, z = 169..171). Pathing exploits
//! that linear shape: the engine first sidesteps in z to match the
//! target row, then walks x until it reaches the target x. Every
//! intermediate cell is checked against `is_drivable_cell` so the
//! call rejects targets where the road has a gap or where a future
//! map adds a branch the engine can't follow. The path stores at
//! most `ENGINE_PATH_CAP` cells which is plenty for this 110-cell
//! road; bumping the road into a real network would justify a real
//! BFS later.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics;

use crate::mathlib::sqrt;
use crate::terrain::{terrain_height, FOOT_MAX, TOWN_Z};
use crate::{M_EMBER, M_ENGINE_BODY, M_ENGINE_HOSE, M_FIRE, M_HELI_PAD, M_ROAD_DIRT};

// ── Tuning ────────────────────────────────────────────────────────

const ENGINE_SX: u8 = 3;
const ENGINE_SY: u8 = 2;
const ENGINE_SZ: u8 = 3;

/// Engine drives at firetruck-travel speed — fast enough that the
/// player isn't waiting forever for it to cross the map.
const ENGINE_SPEED: f32 = 0.40;

/// Snuff radius around the parked engine. Cells inside this radius
/// (Chebyshev distance) get scrubbed each tick.
pub(crate) const HOSE_RADIUS: i32 = 4;

/// How long (in ticks) the engine sits on station with no fire in
/// range before deciding the job's done and returning home. 300
/// ticks ≈ 5 s at 60 fps — short enough that the engine recycles to
/// the next dispatch quickly, long enough that a one-tick flare-up
/// doesn't false-trigger the return.
const NO_FIRE_TIMEOUT: u32 = 300;

/// Max cells in a single road path. The current road is 110 cells;
/// 128 leaves room for a longer or branched network without code
/// changes.
const ENGINE_PATH_CAP: usize = 128;

// ── State machine ─────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq)]
enum EngineState {
    /// Parked at home, available for dispatch.
    Idle,
    /// Driving along `path` toward the target cell.
    Traveling,
    /// At the target cell. Snuff fire in radius; track no-fire time.
    Parked,
    /// Driving back to home along a freshly-computed path.
    ReturningHome,
}

pub(crate) struct FireEngine {
    actor: ActorId,
    pos:   Vec3,
    home:  (u32, u32),
    state: EngineState,
    /// Current path the engine is walking. `None` entries past the
    /// live portion. Index 0 is the next cell to step onto from the
    /// current position.
    path:     [Option<(u32, u32)>; ENGINE_PATH_CAP],
    path_idx: u16,
    path_len: u16,
    /// Ticks of "no fire in range" while parked. Reset to 0 the
    /// moment a fire cell shows up.
    no_fire_ticks: u32,
    /// Cell the engine is currently driving toward for the HUD's
    /// in-progress order badge.
    target: Option<(u32, u32)>,
}

impl FireEngine {
    pub(crate) fn despawn_actor(&self) {
        actor_despawn(self.actor);
    }

    pub(crate) fn init(spawn_x: u32, spawn_z: u32) -> Self {
        let actor = actor_spawn().expect("engine actor pool full");
        let y = terrain_height(spawn_x, spawn_z) as f32;
        let pos = Vec3::new(spawn_x as f32 + 0.5, y, spawn_z as f32 + 0.5);
        let e = Self {
            actor,
            pos,
            home: (spawn_x, spawn_z),
            state: EngineState::Idle,
            path: [None; ENGINE_PATH_CAP],
            path_idx: 0,
            path_len: 0,
            no_fire_ticks: 0,
            target: None,
        };
        e.paint_body();
        e.sync_actor();
        e
    }

    /// 3×3 red chassis + small dark hose nozzle on the top centre,
    /// extended one voxel forward (+Z) so the silhouette reads as a
    /// fire-engine rather than another crew vehicle.
    fn paint_body(&self) {
        for dz in 0..ENGINE_SZ {
            for dx in 0..ENGINE_SX {
                actor_set_voxel(self.actor, U8Vec3::new(dx, 0, dz), M_ENGINE_BODY);
            }
        }
        actor_set_voxel(
            self.actor,
            U8Vec3::new(ENGINE_SX / 2, 1, ENGINE_SZ / 2),
            M_ENGINE_HOSE,
        );
        actor_set_voxel(
            self.actor,
            U8Vec3::new(ENGINE_SX / 2, 1, ENGINE_SZ - 1),
            M_ENGINE_HOSE,
        );
    }

    fn sync_actor(&self) {
        actor_set_position(
            self.actor,
            Vec3::new(
                self.pos.x - (ENGINE_SX as f32) * 0.5,
                self.pos.y,
                self.pos.z - (ENGINE_SZ as f32) * 0.5,
            ),
        );
    }

    pub(crate) fn is_idle(&self) -> bool { matches!(self.state, EngineState::Idle) }

    /// Cell the engine is heading toward — drives the queue-badge
    /// painter. `Some` while travelling or parked; `None` once it's
    /// returning home or back at base.
    pub(crate) fn active_target(&self) -> Option<(u32, u32)> {
        match self.state {
            EngineState::Traveling | EngineState::Parked => self.target,
            _ => None,
        }
    }

    /// Try to dispatch this engine to `target`. Computes a road path
    /// from the current position; returns `false` if the target
    /// isn't a road cell or no path exists. Already-busy engines
    /// reject the order outright so the queue can hand it to the
    /// next free engine.
    pub(crate) fn issue_target(&mut self, target: UVec3) -> bool {
        if !self.is_idle() { return false; }
        let start = (self.pos.x as u32, self.pos.z as u32);
        let tgt = (target.x, target.z);
        if !is_drivable_cell(tgt.0, tgt.1) { return false; }
        let path = match compute_road_path(start, tgt) {
            Some(p) => p,
            None    => return false,
        };
        self.path = path.cells;
        self.path_idx = 0;
        self.path_len = path.len;
        self.target = Some(tgt);
        self.state = EngineState::Traveling;
        true
    }

    pub(crate) fn tick(&mut self) {
        match self.state {
            EngineState::Idle => {
                self.sync_actor();
            }
            EngineState::Traveling => {
                if self.advance_along_path() {
                    // Arrived at target — park.
                    self.state = EngineState::Parked;
                    self.no_fire_ticks = 0;
                }
                self.run_hose_if_parked();
                self.sync_actor();
            }
            EngineState::Parked => {
                let cleared_any = self.run_hose();
                if cleared_any {
                    self.no_fire_ticks = 0;
                } else {
                    self.no_fire_ticks += 1;
                    if self.no_fire_ticks >= NO_FIRE_TIMEOUT {
                        self.begin_return_home();
                    }
                }
                self.sync_actor();
            }
            EngineState::ReturningHome => {
                if self.advance_along_path() {
                    self.state = EngineState::Idle;
                    self.target = None;
                }
                self.sync_actor();
            }
        }
    }

    fn run_hose_if_parked(&mut self) {
        // No-op while travelling; only Parked engines spray.
    }

    /// Snuff M_FIRE / M_EMBER cells within `HOSE_RADIUS` of the
    /// engine. Returns true iff at least one cell was cleared this
    /// tick. The radius is Chebyshev (square footprint) — simpler
    /// than circle and indistinguishable at voxel scale.
    fn run_hose(&self) -> bool {
        let cx = self.pos.x as i32;
        let cz = self.pos.z as i32;
        let mut any = false;
        for dz in -HOSE_RADIUS..=HOSE_RADIUS {
            for dx in -HOSE_RADIUS..=HOSE_RADIUS {
                let x = cx + dx;
                let z = cz + dz;
                if x < 0 || z < 0 { continue; }
                let x = x as u32;
                let z = z as u32;
                if x >= FOOT_MAX || z >= FOOT_MAX { continue; }
                let h = terrain_height(x, z);
                // Sweep the full column above terrain — pines
                // reach `terrain + 8`, so a 4-cell sweep leaves the
                // upper canopy burning. With the cart-side long-burn
                // loop holding each fire cell alight for 6-15 s,
                // missed cells reignite spread instead of going out.
                for y in h..h + 10 {
                    let m = physics::material_at(x, y, z);
                    if m == M_FIRE {
                        crate::extinguish_fire_cell(UVec3::new(x, y, z));
                        any = true;
                    } else if m == M_EMBER {
                        set_voxel(UVec3::new(x, y, z), 0);
                        any = true;
                    }
                }
            }
        }
        any
    }

    fn begin_return_home(&mut self) {
        let start = (self.pos.x as u32, self.pos.z as u32);
        if start == self.home {
            self.state = EngineState::Idle;
            self.target = None;
            return;
        }
        match compute_road_path(start, self.home) {
            Some(path) => {
                self.path = path.cells;
                self.path_idx = 0;
                self.path_len = path.len;
                self.state = EngineState::ReturningHome;
            }
            None => {
                // Stranded — no path home. Just sit idle in place
                // (player can dispatch elsewhere; the engine doesn't
                // get destroyed for this).
                self.state = EngineState::Idle;
                self.target = None;
            }
        }
    }

    /// Step toward `path[path_idx]` at ENGINE_SPEED. On arrival,
    /// advance `path_idx`. Returns true iff the engine just consumed
    /// the last waypoint (i.e., reached the path's end).
    fn advance_along_path(&mut self) -> bool {
        let idx = self.path_idx as usize;
        if idx >= self.path_len as usize {
            return true;
        }
        let Some((tx, tz)) = self.path[idx] else { return true; };
        let tx_f = tx as f32 + 0.5;
        let tz_f = tz as f32 + 0.5;
        let dx = tx_f - self.pos.x;
        let dz = tz_f - self.pos.z;
        let d = sqrt(dx * dx + dz * dz);
        if d < 0.5 {
            self.path_idx += 1;
            // Snap exactly so float drift doesn't accumulate.
            self.pos.x = tx_f;
            self.pos.z = tz_f;
            self.pos.y = terrain_height(tx, tz) as f32;
            return self.path_idx as usize >= self.path_len as usize;
        }
        let step = ENGINE_SPEED.min(d);
        self.pos.x += dx / d * step;
        self.pos.z += dz / d * step;
        let xi = self.pos.x as u32;
        let zi = self.pos.z as u32;
        self.pos.y = terrain_height(xi, zi) as f32;
        false
    }
}

// ── Helpers ───────────────────────────────────────────────────────

/// Is `(x, z)` a cell the engine is allowed to drive onto? Cells
/// painted with M_ROAD_DIRT or M_HELI_PAD on the terrain cap count.
pub(crate) fn is_drivable_cell(x: u32, z: u32) -> bool {
    if x >= FOOT_MAX || z >= FOOT_MAX { return false; }
    let h = terrain_height(x, z);
    if h == 0 { return false; }
    let m = physics::material_at(x, h - 1, z);
    m == M_ROAD_DIRT || m == M_HELI_PAD
}

struct RoadPath {
    cells: [Option<(u32, u32)>; ENGINE_PATH_CAP],
    len:   u16,
}

/// Build a path of drivable cells from `start` to `target`. Walks
/// z first to align with the target's row, then x. Every cell along
/// the way must be drivable; returns `None` on a gap.
///
/// This exploits the cart's currently-linear road layout. A branched
/// road network would justify a real BFS; for now the linear shape
/// gives us a path in two while-loops and zero allocations.
fn compute_road_path(start: (u32, u32), target: (u32, u32)) -> Option<RoadPath> {
    if !is_drivable_cell(target.0, target.1) { return None; }
    let mut cells: [Option<(u32, u32)>; ENGINE_PATH_CAP] = [None; ENGINE_PATH_CAP];
    let mut i: usize = 0;
    let (mut cx, mut cz) = start;

    // If start isn't itself drivable, the engine is somewhere it
    // shouldn't be — bail rather than silently teleport.
    if !is_drivable_cell(cx, cz) { return None; }

    // Step z toward target_z, staying on drivable cells. For the
    // current linear road both endpoints share z=TOWN_Z so this
    // loop is usually a no-op; matters once `target_z` is on a
    // parallel road row.
    while cz != target.1 {
        let next = if cz < target.1 { cz + 1 } else { cz - 1 };
        if !is_drivable_cell(cx, next) { return None; }
        if i >= ENGINE_PATH_CAP { return None; }
        cz = next;
        cells[i] = Some((cx, cz));
        i += 1;
    }

    // Step x toward target_x.
    while cx != target.0 {
        let next = if cx < target.0 { cx + 1 } else { cx - 1 };
        if !is_drivable_cell(next, cz) { return None; }
        if i >= ENGINE_PATH_CAP { return None; }
        cx = next;
        cells[i] = Some((cx, cz));
        i += 1;
    }

    // Final sanity: we landed on target.
    if (cx, cz) != target { return None; }
    Some(RoadPath { cells, len: i as u16 })
}

/// Test helper for the dispatch-time validity check: would a fresh
/// engine standing at the heli-pad row find a path to `target`?
/// Lets `Roster::dispatch_engine_park` reject off-road clicks before
/// queuing them.
pub(crate) fn target_reachable_from(start: (u32, u32), target: UVec3) -> bool {
    compute_road_path(start, (target.x, target.z)).is_some()
}

#[allow(dead_code)]
fn _town_z_used() -> u32 { TOWN_Z }
