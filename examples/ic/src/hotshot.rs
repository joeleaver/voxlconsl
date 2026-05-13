//! Hot-shot crews — parachuting firefighter teams.
//!
//! A hot-shot order is a polyline of waypoints (just like the
//! firetruck's fire-line). When the order pops off the queue, a
//! light drop plane flies in from off-map, drops a parachute at
//! the line's first waypoint (with a small random scatter), and
//! the chute descends to the ground. On touchdown the parachute
//! despawns and a `HotShot` figure appears on the landing cell.
//!
//! The hot-shot then walks to `path[0]`, lays firebreak along the
//! polyline (same `lay_firebreak` semantics as the firetruck), and
//! finishes by standing on the last point in state `AwaitingPickup`.
//! `Roster::tick` looks for any idle helicopter and assigns it to
//! extract the crew; the heli flies to the cell, picks up the
//! hot-shot (despawning the figure), and returns to its pad. If no
//! heli becomes idle within `PICKUP_TIMEOUT_TICKS`, the crew
//! transitions to `WalkingHome` and treads back to base by foot,
//! treating fire cells as obstacles (perpendicular sidestep, same
//! pattern as the truck's slope check). A crew surrounded by fire
//! with no escape route eventually dies of stuck-timeout.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics;

use crate::mathlib::sqrt;
use crate::rng::Rng;
use crate::terrain::{terrain_height, FOOT_MAX, FOOT_MIN};
use crate::units::CREW_PATH_CAP;
use crate::{
    M_EMBER, M_FIRE, M_FIREBREAK_DIRT, M_HOTSHOT_BODY, M_HOTSHOT_HELMET,
    M_HOTSHOT_STRIPE, M_PARACHUTE, M_PINE_LEAVES, M_PINE_WOOD,
    M_TANKER_BODY, M_TANKER_WING,
};

// ── Tuning ────────────────────────────────────────────────────────

const HOTSHOT_WALK_SPEED:  f32 = 0.30;
const HOTSHOT_LAY_SPEED:   f32 = 0.10;
const MAX_SLOPE_DELTA: i32 = 2;
/// Consecutive blocked-step ticks before the hot-shot gives up on
/// movement. ~3 s at 60 fps so an encircled crew eventually dies.
const STUCK_LIMIT: u8 = 180;
const BREAK_HALF_WIDTH: i32 = 1;

/// How long an AwaitingPickup crew sits before giving up and walking
/// home. 600 ticks ≈ 10 s at 60 fps — the player has roughly one
/// heli-refill cycle to free up an aircraft.
const PICKUP_TIMEOUT_TICKS: u32 = 600;

/// Random scatter (in cells) applied to the parachute landing
/// position. Small enough that the crew can walk to `path[0]` in a
/// reasonable amount of time but visibly non-deterministic so two
/// drops on the same target don't land identically.
const LANDING_SCATTER: i32 = 4;

/// Crews dropped per hot-shot order. A single drop plane carries the
/// whole squad and releases them back-to-back as it crosses the
/// target. Each one applies its own random scatter so they land in a
/// loose cluster instead of stacking.
pub(crate) const SQUAD_SIZE: u8 = 4;

/// Spacing (in cells of plane Z progress) between successive
/// parachute releases. With `DROP_PLANE_SPEED = 0.85` this works out
/// to roughly 5 ticks between drops — fast enough that the squad
/// arrives as a single event, slow enough that the chutes don't
/// overlap mid-air.
const DROP_SPACING_CELLS: f32 = 4.0;

// ── Drop plane ────────────────────────────────────────────────────

const DROP_PLANE_SX: u8 = 3;
const DROP_PLANE_SY: u8 = 2;
const DROP_PLANE_SZ: u8 = 5;
const DROP_PLANE_VOL_BYTES: usize =
    (DROP_PLANE_SX as usize) * (DROP_PLANE_SY as usize) * (DROP_PLANE_SZ as usize);
const DROP_PLANE_PREFAB: PrefabId = PrefabId(75);
const DROP_PLANE_ALT:   f32 = 26.0;
const DROP_PLANE_SPEED: f32 = 0.85;
const DROP_PLANE_OFF_MAP_BUF:     f32 = 10.0;
const DROP_PLANE_MIN_APPROACH:    f32 = 30.0;

static mut DROP_PLANE_DENSE: [u8; DROP_PLANE_VOL_BYTES] = [0; DROP_PLANE_VOL_BYTES];

/// One-time registration of the drop plane prefab. Called from the
/// cart's `init` before any plane spawns.
pub(crate) fn init_drop_plane_prefab() {
    unsafe {
        let dense = &mut *(&raw mut DROP_PLANE_DENSE);
        // Y=0 silhouette: short fuselage spine + wide stubby wings +
        // yellow nose tip so the player can tell the personnel
        // carrier from the bombers at a glance.
        for z in 0..DROP_PLANE_SZ {
            put_voxel(dense, 1, 0, z, M_TANKER_BODY);
        }
        // Wings: 3-wide span at z=2.
        put_voxel(dense, 0, 0, 2, M_TANKER_WING);
        put_voxel(dense, 2, 0, 2, M_TANKER_WING);
        // Nose stripe (yellow) — identifies the plane as the drop
        // carrier vs the cyan / pink tanker noses.
        put_voxel(dense, 1, 0, DROP_PLANE_SZ - 1, M_HOTSHOT_STRIPE);
        // Vertical fin — single cell above the tail.
        put_voxel(dense, 1, 1, 0, M_TANKER_BODY);

        prefab_define(
            DROP_PLANE_PREFAB,
            &*(&raw const DROP_PLANE_DENSE),
            U8Vec3::new(DROP_PLANE_SX, DROP_PLANE_SY, DROP_PLANE_SZ),
        );
    }
}

#[inline]
fn put_voxel(dense: &mut [u8; DROP_PLANE_VOL_BYTES], x: u8, y: u8, z: u8, m: u8) {
    let i = ((z as usize) * (DROP_PLANE_SY as usize) + y as usize)
        * (DROP_PLANE_SX as usize) + x as usize;
    dense[i] = m;
}

pub(crate) struct DropPlane {
    actor:   ActorId,
    center:  Vec3,
    target:  UVec3,
    has_been_on_map: bool,
    /// Crews still to release. Counts down from SQUAD_SIZE; each tick
    /// that center.z passes `next_drop_cz` releases one parachute and
    /// advances the schedule.
    drops_remaining: u8,
    next_drop_cz:    f32,
    /// Polyline the hot-shot crew should walk + lay once they touch
    /// down. Carried on the plane so spawn-time data flows through to
    /// the parachute → hotshot pipeline without parallel arrays.
    pub path: [Option<(u32, u32)>; CREW_PATH_CAP],
    /// Cell the crew should head to if they end up walking home.
    pub home: (u32, u32),
}

impl DropPlane {
    /// Spawn a sortie heading north (+Z) toward `target`. The plane
    /// releases SQUAD_SIZE parachutes one at a time as it crosses the
    /// target, spread `DROP_SPACING_CELLS` apart so the squad lands
    /// in a loose line that the per-chute scatter then jitters into a
    /// cluster.
    pub(crate) fn spawn(
        target: UVec3,
        path:   [Option<(u32, u32)>; CREW_PATH_CAP],
        home:   (u32, u32),
    ) -> Self {
        let actor = actor_spawn_from(DROP_PLANE_PREFAB, Orientation::Up)
            .expect("drop plane actor pool full");
        let target_cx = target.x as f32 + 0.5;
        let target_cz = target.z as f32 + 0.5;
        let approach = approach_off_map_north(target_cz);
        let cx = target_cx;
        let cz = target_cz - approach;
        let xu = target.x.min(FOOT_MAX - 1);
        let zu = target.z.min(FOOT_MAX - 1);
        let h = terrain_height(xu, zu) as f32;
        let alt = h + DROP_PLANE_ALT;
        let center = Vec3::new(cx, alt, cz);
        // Centre the squad along the flight axis on the target cell:
        // first chute releases a few cells *before* target_cz, last
        // chute releases a few cells *after*.
        let first_drop_offset = -(SQUAD_SIZE as f32 - 1.0) * 0.5 * DROP_SPACING_CELLS;
        let next_drop_cz = target_cz + first_drop_offset;
        let plane = Self {
            actor,
            center,
            target,
            has_been_on_map: false,
            drops_remaining: SQUAD_SIZE,
            next_drop_cz,
            path,
            home,
        };
        plane.sync_actor();
        plane
    }

    /// Advance one tick. Returns `(alive, drop_cell)`:
    /// - `alive=false` means the plane cleared the map — caller
    ///   despawns the actor.
    /// - `drop_cell=Some(target)` on each frame the plane releases
    ///   another parachute (up to SQUAD_SIZE total per sortie).
    pub(crate) fn tick(&mut self) -> (bool, Option<UVec3>) {
        self.center.z += DROP_PLANE_SPEED;
        let xi = self.center.x as i32;
        let zi = self.center.z as i32;
        if xi >= 0 && (xi as u32) < FOOT_MAX && zi >= 0 && (zi as u32) < FOOT_MAX {
            self.center.y = terrain_height(xi as u32, zi as u32) as f32 + DROP_PLANE_ALT;
        }
        let mut drop_cell: Option<UVec3> = None;
        if self.drops_remaining > 0 && self.center.z >= self.next_drop_cz {
            drop_cell = Some(self.target);
            self.drops_remaining -= 1;
            self.next_drop_cz += DROP_SPACING_CELLS;
        }
        self.sync_actor();
        let on_map = self.center.x >= FOOT_MIN as f32 - DROP_PLANE_OFF_MAP_BUF
            && self.center.x < FOOT_MAX as f32 + DROP_PLANE_OFF_MAP_BUF
            && self.center.z >= FOOT_MIN as f32 - DROP_PLANE_OFF_MAP_BUF
            && self.center.z < FOOT_MAX as f32 + DROP_PLANE_OFF_MAP_BUF;
        if on_map { self.has_been_on_map = true; }
        let alive = !(self.has_been_on_map && !on_map);
        (alive, drop_cell)
    }

    fn sync_actor(&self) {
        actor_set_position(
            self.actor,
            Vec3::new(
                self.center.x - (DROP_PLANE_SX as f32) * 0.5,
                self.center.y,
                self.center.z - (DROP_PLANE_SZ as f32) * 0.5,
            ),
        );
    }

    pub(crate) fn despawn_actor(&self) {
        actor_despawn(self.actor);
    }

    /// Drop altitude — handed to the spawned parachute so its descent
    /// starts at the plane's airspace and falls through ~25 cells of
    /// air before touchdown.
    pub(crate) fn drop_altitude(&self) -> f32 { self.center.y }
}

/// Distance to walk north (-Z direction relative to +Z flight) before
/// `target_cz` is reached, plus the off-map buffer. Always positive.
fn approach_off_map_north(target_cz: f32) -> f32 {
    // Plane flies +Z. To start off-map we need cz = target_cz - approach
    // to be ≤ FOOT_MIN - BUF, i.e. approach ≥ target_cz - FOOT_MIN + BUF.
    let needed = target_cz - FOOT_MIN as f32 + DROP_PLANE_OFF_MAP_BUF;
    needed.max(DROP_PLANE_MIN_APPROACH)
}

// ── Parachute ─────────────────────────────────────────────────────

const PARACHUTE_FALL_SPEED: f32 = 0.35;

pub(crate) struct Parachute {
    actor:        ActorId,
    pos:          Vec3,
    ground_y:     f32,
    landing_cell: (u32, u32),
    pub path: [Option<(u32, u32)>; CREW_PATH_CAP],
    pub home: (u32, u32),
}

impl Parachute {
    /// Spawn a chute at `(landing_cell.x + 0.5, start_alt, landing_cell.z + 0.5)`.
    /// The caller has already applied the scatter offset to `landing_cell`.
    /// `path` + `home` are carried through to the spawned hot-shot crew.
    pub(crate) fn spawn(
        landing_cell: (u32, u32),
        start_alt:    f32,
        path:         [Option<(u32, u32)>; CREW_PATH_CAP],
        home:         (u32, u32),
    ) -> Self {
        let actor = actor_spawn().expect("parachute pool full");
        // Tiny 1×3×1 cluster: helmet voxel + body voxel + fabric voxel
        // stacked vertically. The "fabric" is just the parachute pigment
        // sitting one cell above the body; we don't try to paint a real
        // canopy because we're rendering against an SVO at voxel scale.
        actor_set_voxel(actor, U8Vec3::new(0, 0, 0), M_HOTSHOT_BODY);
        actor_set_voxel(actor, U8Vec3::new(0, 1, 0), M_HOTSHOT_HELMET);
        actor_set_voxel(actor, U8Vec3::new(0, 2, 0), M_PARACHUTE);
        let ground_y = terrain_height(landing_cell.0, landing_cell.1) as f32;
        let pos = Vec3::new(
            landing_cell.0 as f32 + 0.5,
            start_alt,
            landing_cell.1 as f32 + 0.5,
        );
        let p = Self { actor, pos, ground_y, landing_cell, path, home };
        p.sync_actor();
        p
    }

    /// Returns `Some(landing_cell)` the frame the chute touches ground.
    pub(crate) fn tick(&mut self) -> Option<(u32, u32)> {
        self.pos.y -= PARACHUTE_FALL_SPEED;
        if self.pos.y <= self.ground_y {
            return Some(self.landing_cell);
        }
        self.sync_actor();
        None
    }

    fn sync_actor(&self) {
        actor_set_position(
            self.actor,
            Vec3::new(self.pos.x - 0.5, self.pos.y, self.pos.z - 0.5),
        );
    }

    pub(crate) fn despawn_actor(&self) {
        actor_despawn(self.actor);
    }
}

// ── HotShot crew ──────────────────────────────────────────────────

const HOTSHOT_SX: u8 = 1;
const HOTSHOT_SY: u8 = 2;
const HOTSHOT_SZ: u8 = 1;

#[derive(Copy, Clone, PartialEq, Eq)]
enum HotShotState {
    /// Walking toward `path[0]` from the landing cell. No firebreak.
    Walking,
    /// Walking along `path[idx]`, laying firebreak per new cell.
    Laying(u8),
    /// Standing on the last laid cell, waiting for a heli pickup.
    /// Counter ticks toward `PICKUP_TIMEOUT_TICKS`.
    AwaitingPickup,
    /// A heli is en route. The roster's outer sweep despawns the
    /// figure once the heli arrives within `HELI_ARRIVE_R`.
    BeingPicked,
    /// Walking back to base by foot. Fire cells are obstacles
    /// (perpendicular sidestep). `stuck` ticks toward `STUCK_LIMIT`
    /// at which point the crew dies in place.
    WalkingHome,
    /// Crew is done — owner despawns the actor and clears the slot.
    Done,
}

pub(crate) struct HotShot {
    actor:     ActorId,
    pos:       Vec3,
    home:      (u32, u32),
    path:      [Option<(u32, u32)>; CREW_PATH_CAP],
    state:     HotShotState,
    last_cell: Option<(u32, u32)>,
    /// `AwaitingPickup` counts up to PICKUP_TIMEOUT_TICKS; anywhere
    /// else this re-purposes as a stuck counter.
    counter:   u32,
    stuck:     u8,
}

impl HotShot {
    pub(crate) fn spawn(
        landing: (u32, u32),
        home:    (u32, u32),
        path:    [Option<(u32, u32)>; CREW_PATH_CAP],
    ) -> Self {
        let actor = actor_spawn().expect("hotshot actor pool full");
        actor_set_voxel(actor, U8Vec3::new(0, 0, 0), M_HOTSHOT_BODY);
        actor_set_voxel(actor, U8Vec3::new(0, 1, 0), M_HOTSHOT_HELMET);
        let y = terrain_height(landing.0, landing.1) as f32;
        let pos = Vec3::new(landing.0 as f32 + 0.5, y, landing.1 as f32 + 0.5);
        let hs = Self {
            actor,
            pos,
            home,
            path,
            state: HotShotState::Walking,
            last_cell: None,
            counter: 0,
            stuck: 0,
        };
        hs.sync_actor();
        hs
    }

    fn sync_actor(&self) {
        actor_set_position(
            self.actor,
            Vec3::new(
                self.pos.x - (HOTSHOT_SX as f32) * 0.5,
                self.pos.y,
                self.pos.z - (HOTSHOT_SZ as f32) * 0.5,
            ),
        );
    }

    pub(crate) fn cell(&self) -> (u32, u32) {
        (self.pos.x as u32, self.pos.z as u32)
    }

    pub(crate) fn is_awaiting_pickup(&self) -> bool {
        matches!(self.state, HotShotState::AwaitingPickup)
    }

    pub(crate) fn is_being_picked(&self) -> bool {
        matches!(self.state, HotShotState::BeingPicked)
    }

    pub(crate) fn is_done(&self) -> bool {
        matches!(self.state, HotShotState::Done)
    }

    /// Caller (the Roster) sets this when assigning a heli to come
    /// extract the crew. The crew stops counting its pickup timeout
    /// and waits for the heli to arrive.
    pub(crate) fn mark_being_picked(&mut self) {
        if matches!(self.state, HotShotState::AwaitingPickup) {
            self.state = HotShotState::BeingPicked;
            self.counter = 0;
        }
    }

    /// Caller invokes this when the picking heli has arrived.
    /// Transitions to Done; the Roster sweeps the slot next tick.
    pub(crate) fn mark_extracted(&mut self) {
        self.state = HotShotState::Done;
    }

    /// First waypoint of the crew's line — used by the queue-badge
    /// painter so the in-progress order's badge stays pinned to its
    /// anchor cell. None once the crew is past the line.
    pub(crate) fn active_line_head(&self) -> Option<(u32, u32)> {
        match self.state {
            HotShotState::Walking | HotShotState::Laying(_) => {
                self.path[0]
            }
            _ => None,
        }
    }

    /// Short label for the HUD (≤ 4 chars).
    pub(crate) fn state_label(&self) -> &'static str {
        match self.state {
            HotShotState::Walking        => "GO",
            HotShotState::Laying(_)      => "LAY",
            HotShotState::AwaitingPickup => "WAIT",
            HotShotState::BeingPicked    => "PICK",
            HotShotState::WalkingHome    => "HOME",
            HotShotState::Done           => "DONE",
        }
    }

    pub(crate) fn despawn_actor(&self) {
        actor_despawn(self.actor);
    }

    pub(crate) fn tick(&mut self) {
        let (target, speed, do_lay, avoid_fire) = match self.state {
            HotShotState::Walking => {
                let Some(t) = self.path[0] else {
                    self.state = HotShotState::Laying(0);
                    return;
                };
                (t, HOTSHOT_WALK_SPEED, false, false)
            }
            HotShotState::Laying(idx) => {
                match self.path.get(idx as usize).and_then(|s| *s) {
                    Some(t) => (t, HOTSHOT_LAY_SPEED, true, false),
                    None => {
                        // Past the last waypoint — line complete.
                        crate::line_mode::clear_planned_line_voxels(&self.path);
                        self.state = HotShotState::AwaitingPickup;
                        self.counter = 0;
                        self.sync_actor();
                        return;
                    }
                }
            }
            HotShotState::AwaitingPickup => {
                self.counter += 1;
                if self.counter >= PICKUP_TIMEOUT_TICKS {
                    self.state = HotShotState::WalkingHome;
                    self.counter = 0;
                    self.stuck = 0;
                }
                self.sync_actor();
                return;
            }
            HotShotState::BeingPicked => {
                self.sync_actor();
                return;
            }
            HotShotState::WalkingHome => (self.home, HOTSHOT_WALK_SPEED, false, true),
            HotShotState::Done => {
                self.sync_actor();
                return;
            }
        };

        let (tx, tz) = (target.0 as f32 + 0.5, target.1 as f32 + 0.5);
        let dx = tx - self.pos.x;
        let dz = tz - self.pos.z;
        let d = sqrt(dx * dx + dz * dz);

        if d < 0.5 {
            let cur_cell = (self.pos.x as u32, self.pos.z as u32);
            match self.state {
                HotShotState::Walking => {
                    self.lay_firebreak(cur_cell);
                    self.last_cell = Some(cur_cell);
                    self.state = HotShotState::Laying(1);
                }
                HotShotState::Laying(idx) => {
                    let next = idx + 1;
                    if (next as usize) < CREW_PATH_CAP
                        && self.path.get(next as usize).and_then(|s| *s).is_some()
                    {
                        self.state = HotShotState::Laying(next);
                    } else {
                        crate::line_mode::clear_planned_line_voxels(&self.path);
                        self.state = HotShotState::AwaitingPickup;
                        self.counter = 0;
                    }
                }
                HotShotState::WalkingHome => {
                    self.state = HotShotState::Done;
                }
                _ => {}
            }
            self.stuck = 0;
            self.sync_actor();
            return;
        }

        let step_len = speed.min(d);
        let dir_x = dx / d;
        let dir_z = dz / d;
        match self.try_step(dir_x, dir_z, step_len, avoid_fire) {
            Some((mx, mz)) => {
                self.stuck = 0;
                self.pos.x += mx;
                self.pos.z += mz;
                let h = terrain_height(self.pos.x as u32, self.pos.z as u32);
                self.pos.y = h as f32;
                if do_lay {
                    let cell = (self.pos.x as u32, self.pos.z as u32);
                    if Some(cell) != self.last_cell {
                        self.last_cell = Some(cell);
                        self.lay_firebreak(cell);
                    }
                }
            }
            None => {
                self.stuck = self.stuck.saturating_add(1);
                if self.stuck > STUCK_LIMIT {
                    // Trapped — die in place. `Done` lets the roster
                    // sweep the slot. Walking-home figures may die
                    // surrounded by fire; laying figures get the same
                    // outcome if their path becomes impassable.
                    if let HotShotState::Laying(_) = self.state {
                        crate::line_mode::clear_planned_line_voxels(&self.path);
                    }
                    self.state = HotShotState::Done;
                    self.stuck = 0;
                }
            }
        }
        self.sync_actor();
    }

    fn try_step(
        &self,
        dir_x: f32,
        dir_z: f32,
        step_len: f32,
        avoid_fire: bool,
    ) -> Option<(f32, f32)> {
        if let Some(m) = self.attempt(dir_x, dir_z, step_len, avoid_fire) { return Some(m); }
        if let Some(m) = self.attempt(-dir_z, dir_x, step_len, avoid_fire) { return Some(m); }
        if let Some(m) = self.attempt(dir_z, -dir_x, step_len, avoid_fire) { return Some(m); }
        None
    }

    fn attempt(
        &self,
        dir_x: f32,
        dir_z: f32,
        step_len: f32,
        avoid_fire: bool,
    ) -> Option<(f32, f32)> {
        let mx = dir_x * step_len;
        let mz = dir_z * step_len;
        let cur_x = self.pos.x as u32;
        let cur_z = self.pos.z as u32;
        let next_x = (self.pos.x + mx) as u32;
        let next_z = (self.pos.z + mz) as u32;
        if next_x == cur_x && next_z == cur_z {
            return Some((mx, mz));
        }
        if next_x >= FOOT_MAX || next_z >= FOOT_MAX { return None; }
        let h_cur  = terrain_height(cur_x, cur_z) as i32;
        let h_next = terrain_height(next_x, next_z) as i32;
        if (h_next - h_cur).abs() > MAX_SLOPE_DELTA { return None; }
        if avoid_fire {
            // Treat fire (and embers, which signal active spread) as
            // impassable. Probe a small column above the target cell
            // so a fire one voxel up still counts.
            let h = h_next as u32;
            for y in h..(h + 3).min(64) {
                let m = physics::material_at(next_x, y, next_z);
                if m == M_FIRE || m == M_EMBER { return None; }
            }
        }
        Some((mx, mz))
    }

    fn lay_firebreak(&self, cell: (u32, u32)) {
        let (cx, cz) = cell;
        for dz in -BREAK_HALF_WIDTH..=BREAK_HALF_WIDTH {
            for dx in -BREAK_HALF_WIDTH..=BREAK_HALF_WIDTH {
                let x = (cx as i32 + dx) as u32;
                let z = (cz as i32 + dz) as u32;
                let h = terrain_height(x, z);
                if h == 0 { continue; }
                set_voxel(UVec3::new(x, h - 1, z), M_FIREBREAK_DIRT);
                for y in h..h + 6 {
                    let m = physics::material_at(x, y, z);
                    if m == M_PINE_WOOD || m == M_PINE_LEAVES
                        || m == M_FIRE || m == M_EMBER
                    {
                        set_voxel(UVec3::new(x, y, z), 0);
                    }
                }
            }
        }
    }
}

// ── Landing scatter RNG ───────────────────────────────────────────
//
// Module-level RNG seeded at boot so each parachute drop has a
// visibly different landing offset. Deterministic across the same
// mission seed because the cart's `scenario::init` ran first.

static mut SCATTER_RNG: Option<Rng> = None;

pub(crate) fn init_scatter_rng(seed: u32) {
    unsafe { SCATTER_RNG = Some(Rng::new(seed ^ 0xBEEF_CAFE)); }
}

/// Sample a random `(dx, dz)` offset in `[-LANDING_SCATTER, LANDING_SCATTER]`
/// for a parachute landing. Falls back to (0, 0) if init was skipped.
pub(crate) fn sample_scatter() -> (i32, i32) {
    unsafe {
        let rng_ref = &mut *(&raw mut SCATTER_RNG);
        match rng_ref {
            Some(rng) => {
                let dx = (rng.next_u32() % (2 * LANDING_SCATTER as u32 + 1)) as i32 - LANDING_SCATTER;
                let dz = (rng.next_u32() % (2 * LANDING_SCATTER as u32 + 1)) as i32 - LANDING_SCATTER;
                (dx, dz)
            }
            None => (0, 0),
        }
    }
}

/// Apply scatter to `target` and clamp to the playable map. Returns
/// the landing cell the parachute should spawn at.
pub(crate) fn scattered_landing(target: UVec3) -> (u32, u32) {
    let (dx, dz) = sample_scatter();
    let x = (target.x as i32 + dx).clamp(FOOT_MIN as i32, FOOT_MAX as i32 - 1) as u32;
    let z = (target.z as i32 + dz).clamp(FOOT_MIN as i32, FOOT_MAX as i32 - 1) as u32;
    (x, z)
}
