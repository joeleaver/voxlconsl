//! Unit roster — Phase 1 ships a single helicopter + a single ground
//! crew. Both are SDK `actor`s with cart-side state machines.
//!
//! ## Helicopter
//!
//! State machine: `Idle` (hover at pad) → `FlyToTarget` (cross map to
//! a drop point) → `Dropping` (paint a water patch + extinguish
//! adjacent fire) → `FlyToWater` (return to the lake to refill) →
//! `Refilling` (brief pause) → loop.
//!
//! Drops are the player's primary extinguish tool. We paint M_WATER
//! into a 5×5 footprint at the target and *also* clear any M_FIRE in
//! a 5×5×4 box around the drop, so the player gets immediate feedback
//! that the dump was effective (without having to wait for the
//! liquid CA to push water through every fire cell).
//!
//! ## Ground crew
//!
//! State machine: `Idle` → `WalkingTo` (slow line march to a target)
//! → `Idle` again. While walking, each newly-entered cell on the
//! crew's path gets its terrain cap converted to M_FIREBREAK_DIRT and
//! any flammable above the column is cleared up to the height + 6 —
//! this is the bulldozer-cuts-a-line effect.
//!
//! Firebreaks are non-flammable, so embers that land on them snuff
//! immediately (see `fire::step_embers`). Walking a perpendicular
//! line in front of the fire is how the player contains it.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics;

use crate::mathlib::{sine, sqrt};
use crate::terrain::{
    terrain_height, HELI_PAD_X, HELI_PAD_Z, LAKE_CX, LAKE_CZ,
};
use crate::{
    M_BUCKET_WATER, M_CREW_BODY, M_CREW_HELMET, M_EMBER, M_FIRE,
    M_FIREBREAK_DIRT, M_HELICOPTER_BODY, M_HELICOPTER_ROTOR, M_PINE_LEAVES,
    M_PINE_WOOD, M_SELECT_MARKER, M_WATER,
};

// ── Unit IDs ──────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum UnitId {
    Heli,
    Crew,
}

// ── Helicopter ────────────────────────────────────────────────────

const HELI_SIZE_X: u8 = 5;
const HELI_SIZE_Y: u8 = 4;
const HELI_SIZE_Z: u8 = 5;
const HELI_ALT:    f32 = 14.0;
const HELI_SPEED:  f32 = 0.7;
const HELI_DROP_RADIUS: i32 = 2;
const HELI_REFILL_TICKS: u8 = 12;
const HELI_DROP_TICKS:   u8 = 8;
/// XZ distance at which "we've arrived" snaps the state machine.
const HELI_ARRIVE_R: f32 = 1.0;

#[derive(Copy, Clone, PartialEq, Eq)]
enum HeliState {
    Idle,
    FlyToTarget,
    Dropping(u8),
    FlyToWater,
    Refilling(u8),
}

pub(crate) struct Helicopter {
    actor:        ActorId,
    pub pos:      Vec3,
    home_xz:      (f32, f32),
    target_xz:    (f32, f32),
    state:        HeliState,
    bucket_full:  bool,
    rotor_phase:  u8,   // for rotor flipbook
}

impl Helicopter {
    pub(crate) fn init() -> Self {
        let actor = actor_spawn().expect("actor pool full");
        let pad_y = terrain_height(HELI_PAD_X, HELI_PAD_Z) as f32 + HELI_ALT;
        let pos = Vec3::new(
            HELI_PAD_X as f32 - HELI_SIZE_X as f32 * 0.5,
            pad_y,
            HELI_PAD_Z as f32 - HELI_SIZE_Z as f32 * 0.5,
        );
        actor_set_position(actor, pos);
        let h = Self {
            actor,
            pos,
            home_xz: (pos.x, pos.z),
            target_xz: (pos.x, pos.z),
            state: HeliState::Idle,
            bucket_full: true,
            rotor_phase: 0,
        };
        h.paint_body();
        h
    }

    /// Build the heli voxel volume on its actor. Static voxels live
    /// at y = 1..3 (body) and y = 3 (rotor cross); the bucket sits
    /// at y = 0 and is repainted in `update_bucket_visual` based on
    /// `bucket_full`.
    fn paint_body(&self) {
        // Cabin: 3×2×3 block in the middle.
        actor_fill_box(
            self.actor,
            U8Vec3::new(1, 1, 1),
            U8Vec3::new(3, 2, 3),
            M_HELICOPTER_BODY,
        );
        // Tail boom: 1×1×2 extending forward (toward +Z).
        actor_set_voxel(self.actor, U8Vec3::new(2, 1, 4), M_HELICOPTER_BODY);
        actor_set_voxel(self.actor, U8Vec3::new(2, 1, 3), M_HELICOPTER_BODY);
        // Static rotor disc — we'll repaint it in `tick_rotor`.
    }

    fn update_bucket_visual(&self) {
        let m = if self.bucket_full { M_BUCKET_WATER } else { 0 };
        actor_fill_box(
            self.actor,
            U8Vec3::new(1, 0, 1),
            U8Vec3::new(3, 0, 3),
            m,
        );
    }

    /// Animate the rotor by stamping a 2-phase blade pattern on the
    /// top of the actor volume.
    fn tick_rotor(&mut self) {
        self.rotor_phase = self.rotor_phase.wrapping_add(1);
        // Clear the rotor plane every frame, then paint one of two
        // cross patterns based on the phase parity.
        actor_fill_box(
            self.actor,
            U8Vec3::new(0, 3, 0),
            U8Vec3::new(4, 3, 4),
            0,
        );
        let blade = M_HELICOPTER_ROTOR;
        if self.rotor_phase & 1 == 0 {
            // Horizontal blade — span along X.
            for x in 0u8..5 { actor_set_voxel(self.actor, U8Vec3::new(x, 3, 2), blade); }
        } else {
            // Diagonal blade — corners of a cross.
            actor_set_voxel(self.actor, U8Vec3::new(0, 3, 0), blade);
            actor_set_voxel(self.actor, U8Vec3::new(1, 3, 1), blade);
            actor_set_voxel(self.actor, U8Vec3::new(2, 3, 2), blade);
            actor_set_voxel(self.actor, U8Vec3::new(3, 3, 3), blade);
            actor_set_voxel(self.actor, U8Vec3::new(4, 3, 4), blade);
            actor_set_voxel(self.actor, U8Vec3::new(0, 3, 4), blade);
            actor_set_voxel(self.actor, U8Vec3::new(1, 3, 3), blade);
            actor_set_voxel(self.actor, U8Vec3::new(3, 3, 1), blade);
            actor_set_voxel(self.actor, U8Vec3::new(4, 3, 0), blade);
        }
    }

    pub(crate) fn issue_drop(&mut self, target: UVec3) {
        // Target is the cell the player wants water on. Heli flies
        // to it; if the bucket is empty we route through the lake
        // first.
        self.target_xz = (target.x as f32, target.z as f32);
        self.state = if self.bucket_full {
            HeliState::FlyToTarget
        } else {
            HeliState::FlyToWater
        };
    }

    /// One simulation tick.
    pub(crate) fn tick(&mut self) {
        self.tick_rotor();
        match self.state {
            HeliState::Idle => {
                // Hover-bob — small vertical oscillation so the heli
                // doesn't look frozen.
                let bob = sine((self.rotor_phase as f32) * 0.1) * 0.25;
                self.pos.y =
                    terrain_height(self.pos.x as u32, self.pos.z as u32) as f32
                    + HELI_ALT + bob;
            }
            HeliState::FlyToTarget => {
                let arrived = self.fly_toward(self.target_xz);
                if arrived {
                    self.state = HeliState::Dropping(HELI_DROP_TICKS);
                }
            }
            HeliState::Dropping(remaining) => {
                if remaining == HELI_DROP_TICKS {
                    self.drop_water();
                }
                let next = remaining - 1;
                if next == 0 {
                    self.bucket_full = false;
                    self.update_bucket_visual();
                    self.state = HeliState::FlyToWater;
                } else {
                    self.state = HeliState::Dropping(next);
                }
            }
            HeliState::FlyToWater => {
                let lake = (LAKE_CX as f32, LAKE_CZ as f32);
                if self.fly_toward(lake) {
                    self.state = HeliState::Refilling(HELI_REFILL_TICKS);
                }
            }
            HeliState::Refilling(remaining) => {
                let next = remaining - 1;
                if next == 0 {
                    self.bucket_full = true;
                    self.update_bucket_visual();
                    // Auto-return to last drop target if we still have one
                    // set, otherwise idle at the pad.
                    if self.target_xz != self.home_xz {
                        self.state = HeliState::FlyToTarget;
                    } else {
                        self.state = HeliState::Idle;
                    }
                } else {
                    self.state = HeliState::Refilling(next);
                }
            }
        }
        actor_set_position(self.actor, self.pos);
    }

    /// Step toward `(tx, tz)` by HELI_SPEED. Returns `true` if we
    /// reached the target this frame. Altitude tracks terrain so
    /// the heli skims at constant clearance.
    fn fly_toward(&mut self, (tx, tz): (f32, f32)) -> bool {
        let dx = tx - self.pos.x;
        let dz = tz - self.pos.z;
        let d = sqrt(dx * dx + dz * dz);
        if d < HELI_ARRIVE_R { return true; }
        let step = HELI_SPEED.min(d);
        self.pos.x += dx / d * step;
        self.pos.z += dz / d * step;
        let g = terrain_height(self.pos.x as u32, self.pos.z as u32) as f32;
        self.pos.y = g + HELI_ALT;
        false
    }

    /// Spawn a 5×5 footprint of water at the heli's XZ and clear
    /// M_FIRE in a 5×5×4 column under it. Players see fire vanish
    /// the moment the drop touches down — water voxels then flow per
    /// the liquid CA for the visual aftermath.
    fn drop_water(&mut self) {
        let cx = self.pos.x as i32 + (HELI_SIZE_X as i32 / 2);
        let cz = self.pos.z as i32 + (HELI_SIZE_Z as i32 / 2);
        for dz in -HELI_DROP_RADIUS..=HELI_DROP_RADIUS {
            for dx in -HELI_DROP_RADIUS..=HELI_DROP_RADIUS {
                let x = (cx + dx) as u32;
                let z = (cz + dz) as u32;
                let h = terrain_height(x, z);
                // Snuff fire in the 4-cell column above terrain.
                for y in h..h + 4 {
                    if physics::material_at(x, y, z) == M_FIRE {
                        set_voxel(UVec3::new(x, y, z), 0);
                    }
                }
                // Paint a water cell ABOVE the surface so the CA
                // settles it onto the terrain rather than overlaying
                // the surface voxel itself (which could blast away
                // useful materials).
                if physics::material_at(x, h, z) == 0 {
                    set_voxel(UVec3::new(x, h, z), M_WATER);
                }
            }
        }
    }
}

// ── Ground crew ───────────────────────────────────────────────────

const CREW_SIZE_Y: u8 = 3;
const CREW_SPEED:  f32 = 0.10;
/// Half-width of the firebreak the crew lays as it walks.
const CREW_BREAK_HALF_WIDTH: i32 = 1;

#[derive(Copy, Clone, PartialEq, Eq)]
enum CrewState {
    Idle,
    WalkingTo,
}

pub(crate) struct GroundCrew {
    actor:     ActorId,
    pub pos:   Vec3,
    target_xz: (f32, f32),
    state:     CrewState,
    last_cell: Option<(u32, u32)>,
}

impl GroundCrew {
    pub(crate) fn init(spawn_x: u32, spawn_z: u32) -> Self {
        let actor = actor_spawn().expect("actor pool full");
        let y = terrain_height(spawn_x, spawn_z) as f32;
        let pos = Vec3::new(spawn_x as f32, y, spawn_z as f32);
        actor_set_position(actor, pos);
        let g = Self {
            actor,
            pos,
            target_xz: (pos.x, pos.z),
            state: CrewState::Idle,
            last_cell: None,
        };
        g.paint_body();
        g
    }

    fn paint_body(&self) {
        actor_set_voxel(self.actor, U8Vec3::new(0, 0, 0), M_CREW_BODY);
        actor_set_voxel(self.actor, U8Vec3::new(0, 1, 0), M_CREW_BODY);
        actor_set_voxel(self.actor, U8Vec3::new(0, 2, 0), M_CREW_HELMET);
    }

    pub(crate) fn issue_move(&mut self, target: UVec3) {
        self.target_xz = (target.x as f32, target.z as f32);
        self.state = CrewState::WalkingTo;
    }

    pub(crate) fn tick(&mut self) {
        if let CrewState::WalkingTo = self.state {
            let (tx, tz) = self.target_xz;
            let dx = tx - self.pos.x;
            let dz = tz - self.pos.z;
            let d = sqrt(dx * dx + dz * dz);
            if d < 0.5 {
                self.state = CrewState::Idle;
            } else {
                let step = CREW_SPEED.min(d);
                self.pos.x += dx / d * step;
                self.pos.z += dz / d * step;
                let h = terrain_height(self.pos.x as u32, self.pos.z as u32);
                self.pos.y = h as f32;

                // Lay firebreak as we enter each new cell.
                let cell = (self.pos.x as u32, self.pos.z as u32);
                if Some(cell) != self.last_cell {
                    self.last_cell = Some(cell);
                    self.lay_firebreak(cell);
                }
            }
        }
        actor_set_position(self.actor, self.pos);
    }

    /// Carve a 3-cell-wide firebreak around `cell`: convert the
    /// terrain cap to M_FIREBREAK_DIRT, clear any flammable above
    /// the column up to height + 6 (chops out trees in the strip).
    /// Cabins are deliberately NOT chopped — the crew leaves
    /// player-owned structures alone.
    fn lay_firebreak(&self, cell: (u32, u32)) {
        let (cx, cz) = cell;
        for dz in -CREW_BREAK_HALF_WIDTH..=CREW_BREAK_HALF_WIDTH {
            for dx in -CREW_BREAK_HALF_WIDTH..=CREW_BREAK_HALF_WIDTH {
                let x = (cx as i32 + dx) as u32;
                let z = (cz as i32 + dz) as u32;
                let h = terrain_height(x, z);
                if h == 0 { continue; }
                // Replace the grass cap.
                set_voxel(UVec3::new(x, h - 1, z), M_FIREBREAK_DIRT);
                // Clear flammables (and fire / embers) standing on
                // the strip. Cabin slots are intentionally not in
                // this list so the crew won't accidentally raze a
                // structure they're walking past.
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

// ── Selection state ───────────────────────────────────────────────

pub(crate) struct Roster {
    pub heli:     Helicopter,
    pub crew:     GroundCrew,
    pub selected: Option<UnitId>,
    select_actor: ActorId,
}

// 5×5 downward-pointing triangle for the selection marker. Row 0 is
// the top of the arrow (the wide base); row 4 is the tip.
const SELECT_W: u8 = 5;
const SELECT_H: u8 = 5;
const SELECT_VOL_BYTES: usize = (SELECT_W as usize) * (SELECT_H as usize) * 1;
const SELECT_PREFAB: PrefabId = PrefabId(66);
const SELECT_BITMAP: [[u8; SELECT_W as usize]; SELECT_H as usize] = [
    [1, 1, 1, 1, 1],
    [0, 1, 1, 1, 0],
    [0, 1, 1, 1, 0],
    [0, 0, 1, 0, 0],
    [0, 0, 1, 0, 0],
];

static mut SELECT_DENSE: [u8; SELECT_VOL_BYTES] = [0; SELECT_VOL_BYTES];

impl Roster {
    pub(crate) fn init() -> Self {
        let heli = Helicopter::init();
        // Spawn the crew on the road just east of the heli pad.
        let crew = GroundCrew::init(HELI_PAD_X + 8, HELI_PAD_Z);

        // Selection marker: a Billboard actor with a 5×5 arrow. Stays
        // crisp at any zoom and never tilts with the camera.
        unsafe {
            let dense = &mut *(&raw mut SELECT_DENSE);
            for (row_idx, row) in SELECT_BITMAP.iter().enumerate() {
                for (col_idx, &on) in row.iter().enumerate() {
                    if on == 0 { continue; }
                    let lx = col_idx;
                    let ly = (SELECT_H as usize - 1) - row_idx;
                    let i = ly * SELECT_W as usize + lx;
                    dense[i] = M_SELECT_MARKER;
                }
            }
            prefab_define(
                SELECT_PREFAB,
                &*(&raw const SELECT_DENSE),
                U8Vec3::new(SELECT_W, SELECT_H, 1),
            );
        }
        let select_actor = actor_spawn_from(SELECT_PREFAB, Orientation::Up)
            .expect("select marker actor spawn");
        actor_set_render_mode(select_actor, ActorRenderMode::Billboard);
        actor_set_visible(select_actor, false);

        Self {
            heli,
            crew,
            selected: None,
            select_actor,
        }
    }

    /// Tick both units.
    pub(crate) fn tick(&mut self) {
        self.heli.tick();
        self.crew.tick();
        self.update_select_marker();
    }

    pub(crate) fn select_nearest(&mut self, cell: UVec3, max_r: f32) {
        let cx = cell.x as f32;
        let cz = cell.z as f32;
        let d_heli = dist_xz(self.heli.pos, cx, cz);
        let d_crew = dist_xz(self.crew.pos, cx, cz);
        // Tighter selection radius than max — both unit centres must
        // be within max_r of the cursor to count.
        let mut best: Option<(UnitId, f32)> = None;
        if d_heli < max_r { best = Some((UnitId::Heli, d_heli)); }
        if d_crew < max_r {
            if best.map_or(true, |(_, d)| d_crew < d) {
                best = Some((UnitId::Crew, d_crew));
            }
        }
        if let Some((id, _)) = best {
            self.selected = Some(id);
        }
    }

    /// Cycle through the roster: None → Heli → Crew → None.
    pub(crate) fn cycle_selection(&mut self) {
        self.selected = match self.selected {
            None              => Some(UnitId::Heli),
            Some(UnitId::Heli) => Some(UnitId::Crew),
            Some(UnitId::Crew) => None,
        };
    }

    /// Order the currently-selected unit to act on the cursor cell.
    pub(crate) fn issue_order(&mut self, cell: UVec3) {
        match self.selected {
            Some(UnitId::Heli) => self.heli.issue_drop(cell),
            Some(UnitId::Crew) => self.crew.issue_move(cell),
            None => {}
        }
    }

    /// Re-position the select-marker actor above the selected unit
    /// (or hide it).
    fn update_select_marker(&self) {
        let (visible, pos) = match self.selected {
            Some(UnitId::Heli) => (
                true,
                Vec3::new(
                    self.heli.pos.x + HELI_SIZE_X as f32 * 0.5,
                    self.heli.pos.y + HELI_SIZE_Y as f32 + 2.0,
                    self.heli.pos.z + HELI_SIZE_Z as f32 * 0.5,
                ),
            ),
            Some(UnitId::Crew) => (
                true,
                Vec3::new(self.crew.pos.x, self.crew.pos.y + CREW_SIZE_Y as f32 + 2.0, self.crew.pos.z),
            ),
            None => (false, Vec3::new(0.0, 0.0, 0.0)),
        };
        actor_set_visible(self.select_actor, visible);
        if visible {
            actor_set_position(self.select_actor, pos);
        }
    }
}

fn dist_xz(p: Vec3, x: f32, z: f32) -> f32 {
    let dx = p.x - x;
    let dz = p.z - z;
    sqrt(dx * dx + dz * dz)
}
