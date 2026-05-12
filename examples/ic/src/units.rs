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
    M_PINE_WOOD, M_WATER,
};

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

    /// Short label for the heli's current state — shown in the HUD's
    /// UNIT section. Constrained to ≤ 4 chars to fit the 32-wide
    /// sidebar at 4 px / glyph.
    pub(crate) fn state_label(&self) -> &'static str {
        match self.state {
            HeliState::Idle           => "IDLE",
            HeliState::FlyToTarget    => "FLY",
            HeliState::Dropping(_)    => "DROP",
            HeliState::FlyToWater     => "RTRN",
            HeliState::Refilling(_)   => "FILL",
        }
    }

    pub(crate) fn bucket_label(&self) -> &'static str {
        if self.bucket_full { "FULL" } else { "EMPT" }
    }

    /// (x, z) of the current go-to target if the heli is acting on an
    /// order, else `None` (idle at the pad).
    pub(crate) fn target_xz(&self) -> Option<(u32, u32)> {
        if self.state == HeliState::Idle { return None; }
        Some((self.target_xz.0 as u32, self.target_xz.1 as u32))
    }

    pub(crate) fn is_idle(&self) -> bool { matches!(self.state, HeliState::Idle) }

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
                    // Drop back to Idle so the Roster can pop the next
                    // water-drop off the queue at the top of the next
                    // tick. The cart's no-cancel rule means in-flight
                    // orders always run to completion, so we never go
                    // straight from Refilling to FlyToTarget anymore.
                    self.state = HeliState::Idle;
                    self.target_xz = self.home_xz;
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
pub(crate) const CREW_PATH_CAP: usize = 8;

#[derive(Copy, Clone, PartialEq, Eq)]
enum CrewState {
    Idle,
    /// Walking toward waypoint `index` of `path`. Advances to
    /// `index + 1` on arrival; goes Idle when the next slot is
    /// `None`.
    Walking(u8),
}

pub(crate) struct GroundCrew {
    actor:     ActorId,
    pub pos:   Vec3,
    /// Polyline the crew is currently working through. `path[0]` is
    /// the current target while walking; cell on arrival rolls the
    /// index forward.
    path:      [Option<(u32, u32)>; CREW_PATH_CAP],
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
            path: [None; CREW_PATH_CAP],
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

    pub(crate) fn state_label(&self) -> &'static str {
        match self.state {
            CrewState::Idle       => "IDLE",
            CrewState::Walking(_) => "WALK",
        }
    }

    /// The waypoint the crew is currently heading toward — drives
    /// the "C  X,Z" line in the HUD ORDERS section.
    pub(crate) fn target_xz(&self) -> Option<(u32, u32)> {
        match self.state {
            CrewState::Walking(i) => self.path.get(i as usize).and_then(|s| *s),
            CrewState::Idle => None,
        }
    }

    pub(crate) fn is_idle(&self) -> bool { matches!(self.state, CrewState::Idle) }

    /// Hand the crew a polyline of waypoints to walk in order.
    /// Empty input clears the path and parks the crew. Slots
    /// beyond `CREW_PATH_CAP` are silently dropped.
    pub(crate) fn issue_path(&mut self, points: &[UVec3]) {
        self.path = [None; CREW_PATH_CAP];
        for (i, p) in points.iter().take(CREW_PATH_CAP).enumerate() {
            self.path[i] = Some((p.x, p.z));
        }
        if self.path[0].is_some() {
            self.state = CrewState::Walking(0);
        } else {
            self.state = CrewState::Idle;
        }
    }

    pub(crate) fn tick(&mut self) {
        if let CrewState::Walking(idx) = self.state {
            let target = match self.path.get(idx as usize).and_then(|s| *s) {
                Some(t) => t,
                None => { self.state = CrewState::Idle; return; }
            };
            let (tx, tz) = (target.0 as f32, target.1 as f32);
            let dx = tx - self.pos.x;
            let dz = tz - self.pos.z;
            let d = sqrt(dx * dx + dz * dz);
            if d < 0.5 {
                // Arrived — advance to the next waypoint, or park.
                let next = idx + 1;
                if (next as usize) < CREW_PATH_CAP && self.path[next as usize].is_some() {
                    self.state = CrewState::Walking(next);
                } else {
                    self.state = CrewState::Idle;
                }
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

// ── Command queue ────────────────────────────────────────────────
//
// The player no longer micro-controls units. Each click pushes an
// order onto a per-type FIFO; whenever a unit goes idle it pops the
// next compatible order off the queue. Limited unit count = the
// dominant gameplay constraint, since orders aren't cancellable —
// once you click, that water drop is committed.

pub(crate) const WATER_DROP_QUEUE_CAP: usize = 16;
pub(crate) const FIRE_LINE_QUEUE_CAP:  usize = 8;

#[derive(Copy, Clone, Default)]
pub(crate) struct FireLinePath {
    pub points: [Option<UVec3>; CREW_PATH_CAP],
    pub count:  u8,
}

impl FireLinePath {
    pub(crate) fn from_slice(src: &[UVec3]) -> Self {
        let mut out = Self::default();
        for (i, p) in src.iter().take(CREW_PATH_CAP).enumerate() {
            out.points[i] = Some(*p);
            out.count    += 1;
        }
        out
    }
    fn as_slice(&self) -> [UVec3; CREW_PATH_CAP] {
        let mut out = [UVec3::ZERO; CREW_PATH_CAP];
        for i in 0..self.count as usize {
            if let Some(p) = self.points[i] { out[i] = p; }
        }
        out
    }
}

pub(crate) struct CommandQueue {
    water:       [Option<UVec3>; WATER_DROP_QUEUE_CAP],
    water_count: u8,
    lines:       [Option<FireLinePath>; FIRE_LINE_QUEUE_CAP],
    line_count:  u8,
}

impl CommandQueue {
    pub(crate) const fn new() -> Self {
        Self {
            water:       [None; WATER_DROP_QUEUE_CAP],
            water_count: 0,
            lines:       [None; FIRE_LINE_QUEUE_CAP],
            line_count:  0,
        }
    }

    /// Append a water drop. Returns false if the queue is full —
    /// the click is dropped on the floor (orders are non-cancellable,
    /// so the alternative would be evicting a queued order the
    /// player already committed to).
    pub(crate) fn push_water(&mut self, cell: UVec3) -> bool {
        if (self.water_count as usize) >= WATER_DROP_QUEUE_CAP { return false; }
        self.water[self.water_count as usize] = Some(cell);
        self.water_count += 1;
        true
    }

    pub(crate) fn push_line(&mut self, line: FireLinePath) -> bool {
        if (self.line_count as usize) >= FIRE_LINE_QUEUE_CAP { return false; }
        self.lines[self.line_count as usize] = Some(line);
        self.line_count += 1;
        true
    }

    fn pop_water(&mut self) -> Option<UVec3> {
        if self.water_count == 0 { return None; }
        let head = self.water[0].take();
        for i in 1..self.water_count as usize {
            self.water[i - 1] = self.water[i];
        }
        self.water_count -= 1;
        self.water[self.water_count as usize] = None;
        head
    }

    fn pop_line(&mut self) -> Option<FireLinePath> {
        if self.line_count == 0 { return None; }
        let head = self.lines[0].take();
        for i in 1..self.line_count as usize {
            self.lines[i - 1] = self.lines[i].take();
        }
        self.line_count -= 1;
        head
    }

    pub(crate) fn pending_total(&self) -> u32 {
        self.water_count as u32 + self.line_count as u32
    }
    pub(crate) fn pending_water(&self) -> u32 { self.water_count as u32 }
    pub(crate) fn pending_lines(&self) -> u32 { self.line_count as u32 }
}

// ── Roster ────────────────────────────────────────────────────────

pub(crate) struct Roster {
    pub heli:  Helicopter,
    pub crew:  GroundCrew,
    pub queue: CommandQueue,
}

impl Roster {
    pub(crate) fn init() -> Self {
        let heli = Helicopter::init();
        // Spawn the crew on the road just east of the heli pad.
        let crew = GroundCrew::init(HELI_PAD_X + 8, HELI_PAD_Z);
        Self { heli, crew, queue: CommandQueue::new() }
    }

    /// Hand out queued orders to idle units, then tick both units.
    /// Idle is the only state in which a unit picks up new work, so
    /// in-flight orders run to completion regardless of what arrives
    /// later in the queue — matches the no-cancel rule.
    pub(crate) fn tick(&mut self) {
        if self.heli.is_idle() {
            if let Some(cell) = self.queue.pop_water() {
                self.heli.issue_drop(cell);
            }
        }
        if self.crew.is_idle() {
            if let Some(line) = self.queue.pop_line() {
                let path = line.as_slice();
                self.crew.issue_path(&path[..line.count as usize]);
            }
        }
        self.heli.tick();
        self.crew.tick();
    }

    pub(crate) fn dispatch_water_drop(&mut self, cell: UVec3) {
        self.queue.push_water(cell);
    }

    pub(crate) fn dispatch_fire_line(&mut self, points: &[UVec3]) {
        if points.is_empty() { return; }
        self.queue.push_line(FireLinePath::from_slice(points));
    }
}

#[allow(dead_code)]
fn _crew_size_hint() -> u8 { CREW_SIZE_Y }
