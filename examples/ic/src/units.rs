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
//! ## Ground crew (firetruck)
//!
//! State machine: `Idle` → `Traveling` (drive fast to the line's
//! first waypoint, no firebreak) → `Laying(idx)` (drive slowly along
//! the polyline, laying firebreak on every entered cell) → `Idle`.
//!
//! Travel speed is several times higher than lay speed so the truck
//! can get into position quickly. The firebreak is only laid on the
//! designated segments — driving to the start of the line leaves no
//! mark on the world.
//!
//! Terrain slope is checked per step: if the cell ahead is more than
//! `MAX_SLOPE_DELTA` voxels above or below the truck's current cell,
//! the direct path is blocked. The truck tries a perpendicular
//! sidestep; if both perpendiculars are also blocked it sits still
//! for up to `STUCK_LIMIT` ticks, then aborts the order. Player-drawn
//! lines that cross a cliff fail visibly instead of stalling forever.
//!
//! Firebreaks are non-flammable, so embers that land on them snuff
//! immediately (see `fire::step_embers`). Routing a perpendicular
//! line in front of the fire is how the player contains it.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics;

use crate::mathlib::{sine, sqrt};
use crate::terrain::{
    terrain_height, HELI_PAD_X, HELI_PAD_Z, LAKE_CX, LAKE_CZ,
};
use crate::terrain::{FOOT_MAX, FOOT_MIN};
use crate::{
    M_BUCKET_WATER, M_CREW_BODY, M_CREW_HELMET, M_EMBER, M_FIRE,
    M_FIREBREAK_DIRT, M_HELICOPTER_BODY, M_HELICOPTER_ROTOR, M_PINE_LEAVES,
    M_PINE_WOOD, M_PLANNED_RETARDANT, M_PLANNED_WATER, M_RETARDANT,
    M_TANKER_BODY, M_TANKER_RETARDANT_STRIPE, M_TANKER_WATER_STRIPE,
    M_TANKER_WING, M_WATER,
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
    /// Flying to extract a waiting hot-shot crew. `hotshot_slot` is
    /// the index into `Roster.hotshots` so the outer sweep can match
    /// the heli to its passenger when it arrives. The target XZ is
    /// the crew's cell (set when the order is issued).
    FlyToPickup { hotshot_slot: u8 },
    /// Returning to the helipad after an extraction. Doesn't refill —
    /// the bucket isn't drained on pickups. Lands as Idle.
    FlyHome,
}

pub(crate) struct Helicopter {
    actor:        ActorId,
    pub pos:      Vec3,
    home_xz:      (f32, f32),
    target_xz:    (f32, f32),
    state:        HeliState,
    bucket_full:  bool,
    rotor_phase:  u8,   // for rotor flipbook
    /// Latched to Some(hotshot_slot) the frame the heli arrives at
    /// a pickup target. The Roster's outer sweep reads this, despawns
    /// the corresponding crew, and clears the flag.
    pickup_arrived: Option<u8>,
}

impl Helicopter {
    /// Spawn at world cell `(pad_x, pad_z)`. The Roster spaces multiple
    /// helis along +X so their volumes don't visually overlap.
    pub(crate) fn init(pad_x: u32, pad_z: u32) -> Self {
        let actor = actor_spawn().expect("actor pool full");
        let pad_y = terrain_height(pad_x, pad_z) as f32 + HELI_ALT;
        let pos = Vec3::new(
            pad_x as f32 - HELI_SIZE_X as f32 * 0.5,
            pad_y,
            pad_z as f32 - HELI_SIZE_Z as f32 * 0.5,
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
            pickup_arrived: None,
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
            HeliState::Idle             => "IDLE",
            HeliState::FlyToTarget      => "FLY",
            HeliState::Dropping(_)      => "DROP",
            HeliState::FlyToWater       => "RTRN",
            HeliState::Refilling(_)     => "FILL",
            HeliState::FlyToPickup { .. } => "PKUP",
            HeliState::FlyHome          => "HOME",
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

    /// XZ cell the heli is currently delivering a player-targeted drop
    /// to. Returns `Some` while in `FlyToTarget` or `Dropping`; `None`
    /// otherwise (Idle, returning to the lake, or refilling). Used by
    /// `queue_markers` so an in-flight order's badge keeps appearing
    /// over its target until the drop actually lands.
    pub(crate) fn active_drop_target(&self) -> Option<(u32, u32)> {
        match self.state {
            HeliState::FlyToTarget | HeliState::Dropping(_) => {
                Some((self.target_xz.0 as u32, self.target_xz.1 as u32))
            }
            _ => None,
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

    /// Dispatch this heli to pick up a waiting hot-shot crew at
    /// `(cx, cz)`. Bucket state is preserved; the heli flies straight
    /// to the cell, hovers briefly while the roster despawns the
    /// crew, then flies home without refilling.
    pub(crate) fn issue_pickup(&mut self, slot: u8, cell: (f32, f32)) {
        self.target_xz = cell;
        self.state = HeliState::FlyToPickup { hotshot_slot: slot };
        self.pickup_arrived = None;
    }

    /// Latched the frame the heli reaches a pickup target. The
    /// Roster reads this to despawn the corresponding hot-shot crew,
    /// then calls `clear_pickup_arrived` to acknowledge the handoff.
    pub(crate) fn pickup_arrived_slot(&self) -> Option<u8> {
        self.pickup_arrived
    }

    pub(crate) fn clear_pickup_arrived(&mut self) {
        self.pickup_arrived = None;
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
            HeliState::FlyToPickup { hotshot_slot } => {
                let arrived = self.fly_toward(self.target_xz);
                if arrived {
                    // Latch the slot for the Roster's sweep; transition
                    // directly to FlyHome so the heli starts heading
                    // back next tick. The crew despawns this same tick
                    // when the roster sweeps `pickup_arrived`.
                    self.pickup_arrived = Some(hotshot_slot);
                    self.state = HeliState::FlyHome;
                }
            }
            HeliState::FlyHome => {
                if self.fly_toward(self.home_xz) {
                    self.state = HeliState::Idle;
                    self.target_xz = self.home_xz;
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
                // Wipe any planning marker that was painted at this
                // cell when the order was queued. The drop zone is
                // wider than HELI_ARRIVE_R so we'll always catch the
                // marker the cart painted at the targeted cell.
                let marker_y = h + PLANNED_WATER_Y_OFFSET;
                if physics::material_at(x, marker_y, z) == M_PLANNED_WATER {
                    set_voxel(UVec3::new(x, marker_y, z), 0);
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

/// Vertical offset above the terrain cap where the planned-water
/// marker floats. Matches `line_mode::PREVIEW_Y_OFFSET` so the two
/// planning markers sit at the same airspace level.
pub(crate) const PLANNED_WATER_Y_OFFSET: u32 = 2;

// ── Air tanker (one-shot fly-by) ─────────────────────────────────
//
// Spawns at the south edge above terrain, flies north along +Z at
// constant altitude, paints a strip of water or retardant across the
// player-targeted cell as it passes overhead, then continues off
// the north edge and despawns. There is no persistent tanker pool
// in the gameplay sense — each call to `dispatch_*_tanker` summons
// a fresh sortie. `MAX_TANKERS` caps the number of in-flight tankers
// so the actor pool can't be drained.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum TankerKind { Water, Retardant }

#[derive(Copy, Clone, PartialEq, Eq)]
enum TankerPhase {
    /// Flying toward the drop window. No painting yet.
    FlyIn,
    /// Inside the drop window — paints a strip perpendicular to the
    /// flight axis on every new cell of progress.
    Dropping,
    /// Past the drop window, cruising to despawn distance.
    FlyOut,
}

const TANKER_SIZE_X: u8 = 5;
/// Bumped from 2 → 3 so the tail fin can be 2 cells tall, giving the
/// plane a recognisable vertical stabiliser when viewed at the
/// cart's overhead tilt.
const TANKER_SIZE_Y: u8 = 3;
const TANKER_SIZE_Z: u8 = 7;
const TANKER_VOL_BYTES: usize =
    (TANKER_SIZE_X as usize) * (TANKER_SIZE_Y as usize) * (TANKER_SIZE_Z as usize);

const PLANE_WATER_PREFAB:     PrefabId = PrefabId(72);
const PLANE_RETARDANT_PREFAB: PrefabId = PrefabId(73);

static mut PLANE_WATER_DENSE:     [u8; TANKER_VOL_BYTES] = [0; TANKER_VOL_BYTES];
static mut PLANE_RETARDANT_DENSE: [u8; TANKER_VOL_BYTES] = [0; TANKER_VOL_BYTES];

/// One-time registration of the two tanker prefabs. Must be called
/// from the cart's `init` before any tanker spawns. Defining as a
/// real prefab (rather than painting an owned actor) keeps the
/// rotated baked volume tightly bounded so the actor's position
/// math stays predictable across orientations.
pub(crate) fn init_tanker_prefabs() {
    unsafe {
        fill_plane_dense(&mut *(&raw mut PLANE_WATER_DENSE),     M_TANKER_WATER_STRIPE);
        fill_plane_dense(&mut *(&raw mut PLANE_RETARDANT_DENSE), M_TANKER_RETARDANT_STRIPE);
        prefab_define(
            PLANE_WATER_PREFAB,
            &*(&raw const PLANE_WATER_DENSE),
            U8Vec3::new(TANKER_SIZE_X, TANKER_SIZE_Y, TANKER_SIZE_Z),
        );
        prefab_define(
            PLANE_RETARDANT_PREFAB,
            &*(&raw const PLANE_RETARDANT_DENSE),
            U8Vec3::new(TANKER_SIZE_X, TANKER_SIZE_Y, TANKER_SIZE_Z),
        );
    }
}

/// Paint the plane silhouette into a 5×3×7 dense buffer. The shape
/// is meant to read as an airplane from the cart's overhead view:
///   - Y=0 carries the top-down silhouette (spine + small tail
///     stabilisers + wide main wings + colour-coded nose tip).
///   - Y=1..2 stack a 2-cell vertical fin at the tail so the plane
///     gets a tail silhouette in the 3/4 perspective view too.
fn fill_plane_dense(dense: &mut [u8; TANKER_VOL_BYTES], nose_stripe: u8) {
    // Fuselage spine — full Z extent at x=2 (centre column).
    for z in 0..TANKER_SIZE_Z {
        put_voxel(dense, 2, 0, z, M_TANKER_BODY);
    }
    // Small horizontal stabilisers at z=1 (small "back wings").
    put_voxel(dense, 1, 0, 1, M_TANKER_WING);
    put_voxel(dense, 3, 0, 1, M_TANKER_WING);
    // Main wings — 5-wide span at z=3..4, mid-fuselage.
    for z in 3..5 {
        put_voxel(dense, 0, 0, z, M_TANKER_WING);
        put_voxel(dense, 1, 0, z, M_TANKER_WING);
        put_voxel(dense, 3, 0, z, M_TANKER_WING);
        put_voxel(dense, 4, 0, z, M_TANKER_WING);
    }
    // Nose tip — overwrites the spine's body colour with the cargo
    // stripe so the player can identify water vs retardant on sight.
    put_voxel(dense, 2, 0, TANKER_SIZE_Z - 1, nose_stripe);
    // Vertical fin: 2 cells tall above the tail point.
    put_voxel(dense, 2, 1, 0, M_TANKER_BODY);
    put_voxel(dense, 2, 1, 1, M_TANKER_BODY);
    put_voxel(dense, 2, 2, 0, M_TANKER_BODY);
}

#[inline]
fn put_voxel(dense: &mut [u8; TANKER_VOL_BYTES], x: u8, y: u8, z: u8, m: u8) {
    let i = ((z as usize) * (TANKER_SIZE_Y as usize) + y as usize)
        * (TANKER_SIZE_X as usize) + x as usize;
    dense[i] = m;
}
/// Cruise altitude above terrain — well above tree-line + heli so
/// the player can see the tanker from far away.
const TANKER_ALT:   f32 = 28.0;
/// Roughly 2× helicopter speed. Tankers cross the map fast.
const TANKER_SPEED: f32 = 0.9;
/// Extra cells past the map edge added to the computed spawn (and
/// the despawn check) so the plane is clearly off-screen at both
/// ends of the sortie.
const TANKER_OFF_MAP_BUF: f32 = 10.0;
/// Fallback approach for degenerate cases (e.g., zero-direction).
const TANKER_MIN_APPROACH: f32 = 30.0;

pub(crate) struct Tanker {
    actor:      ActorId,
    /// Logical plane center in world-cell coords. Y is altitude.
    /// Per-frame motion advances this; the actor's render position
    /// is derived from it via `actor_origin_for` based on
    /// `orientation` (the baked volume's bbox dims depend on the
    /// rotation).
    center:     Vec3,
    /// Drop strip start in world-cell-centre coords (Y is terrain).
    start:      Vec3,
    /// Unit XZ direction along the flight axis.
    dir:        (f32, f32),
    /// Drop strip length in cells.
    length:     f32,
    /// Strip half-width perpendicular to `dir`.
    half_width: i32,
    kind:       TankerKind,
    phase:      TankerPhase,
    /// Last integer progress at which we painted a strip row, so the
    /// painter fires once per cell of forward motion.
    last_progress: i32,
    /// True once the plane center has been inside the map (with the
    /// off-map buffer). Drives the off-map-despawn check: a plane
    /// that's never crossed in (still flying in from off-map) must
    /// stay alive even though its position is off-map.
    has_been_on_map: bool,
    /// Yaw snapped to the nearest cardinal so the painted +Z-facing
    /// plane reads as if it's actually flying along `dir`. Diagonal
    /// `dir` snaps to whichever cardinal dot-product wins.
    orientation: Orientation,
}

impl Tanker {
    /// Spawn a fresh sortie. The plane starts off the map in `-dir`
    /// so the player sees it fly into frame before reaching the
    /// drop window. Approach distance is computed dynamically per
    /// (start, dir) — see `approach_off_map`.
    pub(crate) fn spawn(
        start: UVec3,
        dir: (f32, f32),
        length: u32,
        half_width: i32,
        kind: TankerKind,
    ) -> Self {
        let orientation = snap_yaw_orientation(dir);
        let prefab = match kind {
            TankerKind::Water     => PLANE_WATER_PREFAB,
            TankerKind::Retardant => PLANE_RETARDANT_PREFAB,
        };
        // Spawn pre-baked at the yaw matching `dir`. The host shares
        // the baked volume across tankers with the same (prefab,
        // orientation) so this is cheap.
        let actor = actor_spawn_from(prefab, orientation)
            .expect("tanker actor pool full");
        let start_cx = start.x as f32 + 0.5;
        let start_cz = start.z as f32 + 0.5;
        let approach = approach_off_map(start_cx, start_cz, dir);
        let plane_cx = start_cx - dir.0 * approach;
        let plane_cz = start_cz - dir.1 * approach;
        let h = terrain_h_clamped(start.x as i32, start.z as i32) as f32;
        let alt = h + TANKER_ALT;
        let t = Self {
            actor,
            center: Vec3::new(plane_cx, alt, plane_cz),
            start: Vec3::new(start_cx, h, start_cz),
            dir,
            length: length as f32,
            half_width,
            kind,
            phase: TankerPhase::FlyIn,
            last_progress: i32::MIN,
            has_been_on_map: false,
            orientation,
        };
        actor_set_position(t.actor, actor_origin_for(t.center, orientation));
        t
    }

    /// Advance one tick. Returns `false` once the plane has cleared
    /// the map's edge after having been over the map (the off-map
    /// despawn condition).
    pub(crate) fn tick(&mut self) -> bool {
        self.center.x += self.dir.0 * TANKER_SPEED;
        self.center.z += self.dir.1 * TANKER_SPEED;

        // Track altitude over terrain so the cruise hugs hills.
        let xi = self.center.x as i32;
        let zi = self.center.z as i32;
        if xi >= 0 && (xi as u32) < FOOT_MAX && zi >= 0 && (zi as u32) < FOOT_MAX {
            self.center.y = terrain_height(xi as u32, zi as u32) as f32 + TANKER_ALT;
        }

        // Progress is the signed distance from start along the
        // flight axis — negative while approaching, 0..length while
        // dropping, > length while exiting.
        let dx = self.center.x - self.start.x;
        let dz = self.center.z - self.start.z;
        let progress = dx * self.dir.0 + dz * self.dir.1;

        match self.phase {
            TankerPhase::FlyIn if progress >= 0.0 => {
                self.phase = TankerPhase::Dropping;
            }
            TankerPhase::Dropping if progress > self.length => {
                self.phase = TankerPhase::FlyOut;
            }
            _ => {}
        }

        if self.phase == TankerPhase::Dropping {
            let p_cell = progress as i32;
            if p_cell != self.last_progress {
                self.last_progress = p_cell;
                self.paint_strip(p_cell);
            }
        }

        actor_set_position(self.actor, actor_origin_for(self.center, self.orientation));

        // On-map check with a buffer so the plane stays alive until
        // its full body has cleared the edge. Once it's been
        // on-map at least once, the first time it leaves we
        // despawn — so the sortie enters and exits the map.
        let on_map = self.center.x >= FOOT_MIN as f32 - TANKER_OFF_MAP_BUF
            && self.center.x < FOOT_MAX as f32 + TANKER_OFF_MAP_BUF
            && self.center.z >= FOOT_MIN as f32 - TANKER_OFF_MAP_BUF
            && self.center.z < FOOT_MAX as f32 + TANKER_OFF_MAP_BUF;
        if on_map { self.has_been_on_map = true; }
        !(self.has_been_on_map && !on_map)
    }

    /// Paint one row of the drop strip at integer `progress` along
    /// the flight axis. Row centre is `start + dir * progress`; the
    /// row extends `±half_width` perpendicular to `dir`.
    fn paint_strip(&self, progress: i32) {
        let cxf = self.start.x + self.dir.0 * progress as f32;
        let czf = self.start.z + self.dir.1 * progress as f32;
        // Perpendicular to (dir.0, dir.1) is (-dir.1, dir.0). Width
        // is symmetric so the sign of the perpendicular doesn't
        // matter; the loop covers ±half_width either way.
        let pdx = -self.dir.1;
        let pdz = self.dir.0;
        for w in -self.half_width..=self.half_width {
            let xf = cxf + pdx * w as f32;
            let zf = czf + pdz * w as f32;
            if xf < 0.0 || zf < 0.0 { continue; }
            let xu = xf as u32;
            let zu = zf as u32;
            if xu >= FOOT_MAX || zu >= FOOT_MAX { continue; }
            let h = terrain_height(xu, zu);
            // Snuff fire in the 4-cell column above terrain.
            for y in h..h + 4 {
                if physics::material_at(xu, y, zu) == M_FIRE {
                    set_voxel(UVec3::new(xu, y, zu), 0);
                }
            }
            match self.kind {
                TankerKind::Water => {
                    // Water settles via the liquid CA: paint a cell
                    // above the surface so it spreads onto terrain.
                    if physics::material_at(xu, h, zu) == 0 {
                        set_voxel(UVec3::new(xu, h, zu), M_WATER);
                    }
                    // Clear the cyan planning marker if a heli also
                    // had this cell queued.
                    let marker_y = h + 2;
                    if physics::material_at(xu, marker_y, zu) == M_PLANNED_WATER {
                        set_voxel(UVec3::new(xu, marker_y, zu), 0);
                    }
                }
                TankerKind::Retardant => {
                    // Retardant replaces the terrain cap so it acts
                    // as a true firebreak. Then strip flammables in
                    // the 6-cell column so trees in the drop zone
                    // get cleared.
                    if h > 0 {
                        set_voxel(UVec3::new(xu, h - 1, zu), M_RETARDANT);
                    }
                    for y in h..h + 6 {
                        let m = physics::material_at(xu, y, zu);
                        if m == M_PINE_WOOD || m == M_PINE_LEAVES
                            || m == M_EMBER
                        {
                            set_voxel(UVec3::new(xu, y, zu), 0);
                        }
                    }
                    // Replace the floating preview voxel with air —
                    // it's done its job advertising the drop.
                    let preview_y = h + 2;
                    if physics::material_at(xu, preview_y, zu) == M_PLANNED_RETARDANT {
                        set_voxel(UVec3::new(xu, preview_y, zu), 0);
                    }
                }
            }
        }
    }

    pub(crate) fn despawn_actor(&self) {
        actor_despawn(self.actor);
    }
}

/// Clamp `(x, z)` to in-bounds before sampling terrain. Tanker spawn
/// math can land outside the world bounds along the approach runway;
/// using a clamped sample for spawn altitude keeps the plane from
/// snapping to terrain_height(0,0) when spawning off-map.
fn terrain_h_clamped(x: i32, z: i32) -> u32 {
    let xc = x.clamp(FOOT_MIN as i32, FOOT_MAX as i32 - 1) as u32;
    let zc = z.clamp(FOOT_MIN as i32, FOOT_MAX as i32 - 1) as u32;
    terrain_height(xc, zc)
}

/// Snap an XZ flight direction to the nearest cardinal yaw so the
/// painted +Z-facing plane visually points along `dir`. Diagonal
/// `dir` rounds to whichever axis dominates in magnitude.
fn snap_yaw_orientation(dir: (f32, f32)) -> Orientation {
    let ax = dir.0.abs();
    let az = dir.1.abs();
    if az >= ax {
        // Dominant Z component.
        if dir.1 >= 0.0 { Orientation::Up } else { Orientation::UpRot180 }
    } else {
        // Dominant X component.
        if dir.0 >= 0.0 { Orientation::UpRot90 } else { Orientation::UpRot270 }
    }
}

/// World-space corner position to give `actor_set_position` so the
/// rotated baked volume ends up centred on `center`. The bake
/// permutes the source's (X, Z) extents for 90°/270° rotations, so
/// the half-size we subtract depends on whether `orientation` is a
/// 90° step or not.
fn actor_origin_for(center: Vec3, orientation: Orientation) -> Vec3 {
    let hx_long  = TANKER_SIZE_X as f32 * 0.5;  // 2.5
    let hz_long  = TANKER_SIZE_Z as f32 * 0.5;  // 3.5
    let (hx, hz) = match orientation {
        Orientation::Up | Orientation::UpRot180 => (hx_long, hz_long),
        // 90° / 270°: the baked volume's X-extent is the source's Z
        // (the fuselage length), and its Z-extent is the source's X
        // (the wingspan).
        _ => (hz_long, hx_long),
    };
    Vec3::new(center.x - hx, center.y, center.z - hz)
}

/// Distance to walk from `(cx, cz)` along `-dir` to reach a point
/// off the map's bounding box (in at least one axis), plus
/// `TANKER_OFF_MAP_BUF` extra cells so the plane is well off-screen
/// at spawn. Falls back to `TANKER_MIN_APPROACH` for the degenerate
/// case `dir = (0, 0)`.
fn approach_off_map(cx: f32, cz: f32, dir: (f32, f32)) -> f32 {
    let mut t_min = f32::INFINITY;
    if dir.0.abs() > 1e-3 {
        // Going -dir on X means subtracting `dir.0 * t` from cx.
        // For dir.0 > 0 we'll exit the south/west edge (cx → MIN);
        // for dir.0 < 0 we'll exit the north/east edge (cx → MAX).
        let t = if dir.0 > 0.0 {
            (cx - FOOT_MIN as f32) / dir.0
        } else {
            (FOOT_MAX as f32 - cx) / -dir.0
        };
        if t > 0.0 && t < t_min { t_min = t; }
    }
    if dir.1.abs() > 1e-3 {
        let t = if dir.1 > 0.0 {
            (cz - FOOT_MIN as f32) / dir.1
        } else {
            (FOOT_MAX as f32 - cz) / -dir.1
        };
        if t > 0.0 && t < t_min { t_min = t; }
    }
    if t_min.is_infinite() {
        TANKER_MIN_APPROACH
    } else {
        (t_min + TANKER_OFF_MAP_BUF).max(TANKER_MIN_APPROACH)
    }
}

// ── Ground crew (firetruck) ───────────────────────────────────────

const CREW_SIZE_X: u8 = 3;
const CREW_SIZE_Y: u8 = 2;
const CREW_SIZE_Z: u8 = 3;
/// Speed while driving to the start of a line or returning to base —
/// truck has no work to do but cover distance.
const CREW_TRAVEL_SPEED: f32 = 0.40;
/// Speed while actively laying firebreak along the line. Slow so the
/// player can see the strip get painted and so the crew is a real
/// bottleneck against the fire.
const CREW_LAY_SPEED:    f32 = 0.10;
/// Maximum allowed vertical delta between the truck's current cell
/// and the cell it's about to enter. Anything steeper blocks the step.
const MAX_SLOPE_DELTA: i32 = 2;
/// Consecutive blocked-step ticks before the truck gives up on an
/// order. Roughly 1.5 s at 60 fps — long enough that a small detour
/// works out, short enough that a truly impossible line fails fast.
const STUCK_LIMIT: u8 = 90;
/// Half-width of the firebreak the crew lays as it drives. With
/// `CREW_BREAK_HALF_WIDTH = 1` the strip matches the truck footprint.
const CREW_BREAK_HALF_WIDTH: i32 = 1;
pub(crate) const CREW_PATH_CAP: usize = 8;

#[derive(Copy, Clone, PartialEq, Eq)]
enum CrewState {
    /// Parked. Available to be dispatched. The truck only counts as
    /// idle when it's actually motionless at home (or just spawned).
    Idle,
    /// Driving fast to the line's first waypoint. No firebreak laid.
    Traveling,
    /// Driving slowly toward `path[idx]`, laying firebreak on every
    /// newly-entered cell.
    Laying(u8),
    /// No more orders queued — driving back to the spawn cell at
    /// travel speed. Can be interrupted by a fresh `issue_path`.
    ReturningHome,
}

pub(crate) struct GroundCrew {
    actor:     ActorId,
    /// Logical truck position — cell-centred. Render position is
    /// offset by `-SIZE/2` so the 3×3 actor volume frames `pos`.
    pub pos:   Vec3,
    /// Cell the truck returns to when there's no more work queued.
    home:      (u32, u32),
    /// Polyline the crew is currently working through. `path[0]` is
    /// the line's anchor cell.
    path:      [Option<(u32, u32)>; CREW_PATH_CAP],
    state:     CrewState,
    last_cell: Option<(u32, u32)>,
    /// Ticks of "blocked by slope, couldn't move" in a row. Reset to
    /// 0 on any successful step. Reaching `STUCK_LIMIT` aborts the
    /// current order.
    stuck:     u8,
}

impl GroundCrew {
    pub(crate) fn init(spawn_x: u32, spawn_z: u32) -> Self {
        let actor = actor_spawn().expect("actor pool full");
        let y = terrain_height(spawn_x, spawn_z) as f32;
        // Cell-centred so the 3×3 actor volume frames the spawn cell.
        let pos = Vec3::new(spawn_x as f32 + 0.5, y, spawn_z as f32 + 0.5);
        let g = Self {
            actor,
            pos,
            home: (spawn_x, spawn_z),
            path: [None; CREW_PATH_CAP],
            state: CrewState::Idle,
            last_cell: None,
            stuck: 0,
        };
        g.paint_body();
        g.sync_actor_position();
        g
    }

    /// 3×3 orange chassis with a yellow beacon centred on top.
    fn paint_body(&self) {
        for dz in 0..CREW_SIZE_Z {
            for dx in 0..CREW_SIZE_X {
                actor_set_voxel(self.actor, U8Vec3::new(dx, 0, dz), M_CREW_BODY);
            }
        }
        actor_set_voxel(
            self.actor,
            U8Vec3::new(CREW_SIZE_X / 2, 1, CREW_SIZE_Z / 2),
            M_CREW_HELMET,
        );
    }

    /// Render the actor with its corner offset so `self.pos` ends up
    /// at the centre of the 3×3 footprint.
    fn sync_actor_position(&self) {
        actor_set_position(
            self.actor,
            Vec3::new(
                self.pos.x - (CREW_SIZE_X as f32) * 0.5,
                self.pos.y,
                self.pos.z - (CREW_SIZE_Z as f32) * 0.5,
            ),
        );
    }

    pub(crate) fn state_label(&self) -> &'static str {
        match self.state {
            CrewState::Idle          => "IDLE",
            CrewState::Traveling     => "GO",
            CrewState::Laying(_)     => "LAY",
            CrewState::ReturningHome => "HOME",
        }
    }

    /// The waypoint the crew is currently heading toward — drives
    /// the "C  X,Z" line in the HUD ORDERS section.
    pub(crate) fn target_xz(&self) -> Option<(u32, u32)> {
        match self.state {
            CrewState::Idle => None,
            CrewState::Traveling => self.path[0],
            CrewState::Laying(i) => self.path.get(i as usize).and_then(|s| *s),
            CrewState::ReturningHome => Some(self.home),
        }
    }

    /// Strict-idle: parked at base. Used by HUD busy counts.
    pub(crate) fn is_idle(&self) -> bool { matches!(self.state, CrewState::Idle) }

    /// Available to take a new fire-line order. Includes `Idle` AND
    /// `ReturningHome` — a returning truck can be re-routed mid-trip
    /// because it isn't doing meaningful work yet.
    pub(crate) fn is_available(&self) -> bool {
        matches!(self.state, CrewState::Idle | CrewState::ReturningHome)
    }

    /// First waypoint of the line the crew is currently working on.
    /// `None` if it's idle or returning home. Used by `queue_markers`
    /// to keep the badge pinned to the line's anchor cell from
    /// dispatch through completion.
    pub(crate) fn active_line_head(&self) -> Option<(u32, u32)> {
        match self.state {
            CrewState::Traveling | CrewState::Laying(_) => self.path[0],
            CrewState::Idle | CrewState::ReturningHome => None,
        }
    }

    /// If the crew is currently parked-but-not-at-home, send it back
    /// to its spawn cell. Called by `Roster::tick` when an idle crew
    /// finds no queued work.
    pub(crate) fn send_home_if_needed(&mut self) {
        if !matches!(self.state, CrewState::Idle) { return; }
        let cell = (self.pos.x as u32, self.pos.z as u32);
        if cell != self.home {
            self.state = CrewState::ReturningHome;
            self.stuck = 0;
        }
    }

    /// Hand the crew a polyline of waypoints to drive. Empty input
    /// clears the path and parks the crew. Slots beyond `CREW_PATH_CAP`
    /// are silently dropped. Calling this while the truck is returning
    /// home interrupts the trip and dispatches the new order.
    pub(crate) fn issue_path(&mut self, points: &[UVec3]) {
        self.path = [None; CREW_PATH_CAP];
        for (i, p) in points.iter().take(CREW_PATH_CAP).enumerate() {
            self.path[i] = Some((p.x, p.z));
        }
        if self.path[0].is_some() {
            self.state = CrewState::Traveling;
        } else {
            self.state = CrewState::Idle;
        }
        self.last_cell = None;
        self.stuck = 0;
    }

    pub(crate) fn tick(&mut self) {
        // Pick the current target, speed, and whether to lay firebreak.
        let (target, speed, do_lay) = match self.state {
            CrewState::Idle => {
                self.sync_actor_position();
                return;
            }
            CrewState::Traveling => match self.path[0] {
                Some(t) => (t, CREW_TRAVEL_SPEED, false),
                None => {
                    self.state = CrewState::Idle;
                    self.sync_actor_position();
                    return;
                }
            },
            CrewState::Laying(idx) => match self.path.get(idx as usize).and_then(|s| *s) {
                Some(t) => (t, CREW_LAY_SPEED, true),
                None => {
                    // Past the last waypoint — line complete.
                    self.state = CrewState::Idle;
                    crate::line_mode::clear_planned_line_voxels(&self.path);
                    self.sync_actor_position();
                    return;
                }
            },
            CrewState::ReturningHome => (self.home, CREW_TRAVEL_SPEED, false),
        };

        // Target cell centre, so `d < 0.5` means the truck is on the
        // target cell.
        let (tx, tz) = (target.0 as f32 + 0.5, target.1 as f32 + 0.5);
        let dx = tx - self.pos.x;
        let dz = tz - self.pos.z;
        let d = sqrt(dx * dx + dz * dz);

        if d < 0.5 {
            // Arrived — transition.
            let cur_cell = (self.pos.x as u32, self.pos.z as u32);
            match self.state {
                CrewState::Traveling => {
                    // Start laying. Drop a firebreak at the anchor
                    // cell so path[0] itself ends up dug.
                    self.lay_firebreak(cur_cell);
                    self.last_cell = Some(cur_cell);
                    self.state = CrewState::Laying(1);
                }
                CrewState::Laying(idx) => {
                    let next = idx + 1;
                    if (next as usize) < CREW_PATH_CAP
                        && self.path.get(next as usize).and_then(|s| *s).is_some()
                    {
                        self.state = CrewState::Laying(next);
                    } else {
                        self.state = CrewState::Idle;
                        crate::line_mode::clear_planned_line_voxels(&self.path);
                    }
                }
                CrewState::ReturningHome => {
                    self.state = CrewState::Idle;
                }
                CrewState::Idle => {}
            }
            self.stuck = 0;
            self.sync_actor_position();
            return;
        }

        // Movement step with slope-check + perpendicular detour.
        let step_len = speed.min(d);
        let dir_x = dx / d;
        let dir_z = dz / d;
        match self.try_step(dir_x, dir_z, step_len) {
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
                // Slope-blocked in every direction tried. Sit tight,
                // and after STUCK_LIMIT ticks give up so the player
                // isn't soft-locked behind a cliff.
                self.stuck = self.stuck.saturating_add(1);
                if self.stuck > STUCK_LIMIT {
                    let was_laying = matches!(self.state, CrewState::Laying(_));
                    self.state = CrewState::Idle;
                    if was_laying {
                        crate::line_mode::clear_planned_line_voxels(&self.path);
                    }
                    self.stuck = 0;
                }
            }
        }

        self.sync_actor_position();
    }

    /// Try to move `step_len` along `(dir_x, dir_z)`. If the cell
    /// ahead is too steep, try a sidestep 90° left, then 90° right.
    /// Returns the `(dx, dz)` actually applied to `pos`, or `None` if
    /// every option is blocked.
    fn try_step(&self, dir_x: f32, dir_z: f32, step_len: f32) -> Option<(f32, f32)> {
        if let Some(m) = self.attempt(dir_x, dir_z, step_len) { return Some(m); }
        if let Some(m) = self.attempt(-dir_z, dir_x, step_len) { return Some(m); }
        if let Some(m) = self.attempt(dir_z, -dir_x, step_len) { return Some(m); }
        None
    }

    /// Probe a single direction. Sub-cell moves are always allowed;
    /// cell-boundary crossings have to pass the slope check.
    fn attempt(&self, dir_x: f32, dir_z: f32, step_len: f32) -> Option<(f32, f32)> {
        let mx = dir_x * step_len;
        let mz = dir_z * step_len;
        let cur_x = self.pos.x as u32;
        let cur_z = self.pos.z as u32;
        let next_x = (self.pos.x + mx) as u32;
        let next_z = (self.pos.z + mz) as u32;
        if next_x == cur_x && next_z == cur_z {
            return Some((mx, mz));
        }
        let h_cur  = terrain_height(cur_x, cur_z) as i32;
        let h_next = terrain_height(next_x, next_z) as i32;
        if (h_next - h_cur).abs() <= MAX_SLOPE_DELTA {
            Some((mx, mz))
        } else {
            None
        }
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
pub(crate) const HOTSHOT_QUEUE_CAP:    usize = 4;

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
    hotshots:       [Option<FireLinePath>; HOTSHOT_QUEUE_CAP],
    hotshot_count:  u8,
}

impl CommandQueue {
    pub(crate) const fn new() -> Self {
        Self {
            water:       [None; WATER_DROP_QUEUE_CAP],
            water_count: 0,
            lines:       [None; FIRE_LINE_QUEUE_CAP],
            line_count:  0,
            hotshots:       [None; HOTSHOT_QUEUE_CAP],
            hotshot_count:  0,
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

    pub(crate) fn push_hotshot(&mut self, line: FireLinePath) -> bool {
        if (self.hotshot_count as usize) >= HOTSHOT_QUEUE_CAP { return false; }
        self.hotshots[self.hotshot_count as usize] = Some(line);
        self.hotshot_count += 1;
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

    fn pop_hotshot(&mut self) -> Option<FireLinePath> {
        if self.hotshot_count == 0 { return None; }
        let head = self.hotshots[0].take();
        for i in 1..self.hotshot_count as usize {
            self.hotshots[i - 1] = self.hotshots[i].take();
        }
        self.hotshot_count -= 1;
        head
    }

    pub(crate) fn pending_total(&self) -> u32 {
        self.water_count as u32 + self.line_count as u32 + self.hotshot_count as u32
    }
    pub(crate) fn pending_water(&self) -> u32 { self.water_count as u32 }
    pub(crate) fn pending_lines(&self) -> u32 { self.line_count as u32 }
    pub(crate) fn pending_hotshots(&self) -> u32 { self.hotshot_count as u32 }

    /// Peek at the i'th queued water drop without removing it.
    /// Used by the on-map badge renderer.
    pub(crate) fn water_at(&self, i: usize) -> Option<UVec3> {
        if i >= self.water_count as usize { return None; }
        self.water[i]
    }

    /// Peek at the i'th queued fire line. Returns the line's first
    /// waypoint as the "anchor cell" the on-map badge points at.
    pub(crate) fn line_head_at(&self, i: usize) -> Option<UVec3> {
        if i >= self.line_count as usize { return None; }
        self.lines[i].as_ref().and_then(|l| l.points[0])
    }

    /// Peek at the i'th queued hotshot order's first waypoint.
    pub(crate) fn hotshot_head_at(&self, i: usize) -> Option<UVec3> {
        if i >= self.hotshot_count as usize { return None; }
        self.hotshots[i].as_ref().and_then(|l| l.points[0])
    }
}

// ── Roster ────────────────────────────────────────────────────────
//
// Multi-unit pool: up to MAX_HELIS helicopters and MAX_CREWS ground
// crews, each in an `Option` slot so the cart can spawn a per-scenario
// count short of the cap. Idle units of either type pull from the
// matching FIFO at the top of each tick — so the queue serialises
// faster when more units are alive.

pub(crate) const MAX_HELIS:   usize = 4;
pub(crate) const MAX_CREWS:   usize = 6;
/// Cap on simultaneous in-flight tankers. Tankers are one-shot
/// sorties (spawn → fly → drop → despawn) so this caps the number
/// of planes the player can have in the air at once, not the total
/// dispatch count.
pub(crate) const MAX_TANKERS: usize = 4;
/// Cap on queued tanker sorties waiting to spawn. Shared between
/// water-tanker and retardant-tanker dispatches.
pub(crate) const TANKER_REQUEST_CAP: usize = 8;
/// Hard cap on concurrent hot-shot crews on the ground. Sized to
/// hold two full squads at SQUAD_SIZE=4. The scenario tier sets the
/// *usable* crew count via `Roster::hotshot_cap` ≤ this.
pub(crate) const MAX_HOTSHOTS:    usize = 8;
/// Hard cap on in-flight drop planes (each ferries a full squad).
pub(crate) const MAX_DROP_PLANES: usize = 2;
/// Hard cap on parachutes mid-descent — one squad's worth, since
/// the chutes are released ~5 ticks apart and the first ones land
/// before the last ones are even released for a second sortie.
pub(crate) const MAX_PARACHUTES:  usize = 4;
/// Frames of "call sign" delay after a tanker order is committed
/// before the plane spawns off-map. Gives the player a beat to see
/// the badge (and preview, for retardant) before the plane appears.
const TANKER_DELAY_TICKS: u32 = 60;

/// Outstanding tanker dispatch waiting in the shared request queue.
/// While `delay > 0`, the plane is "still being scrambled" — the
/// queue badge (and retardant preview line) is visible but no
/// tanker yet. When `delay` reaches 0 and a tanker slot is free,
/// the request gets popped and a real `Tanker` is spawned.
#[derive(Copy, Clone)]
pub(crate) struct TankerRequest {
    /// Strip start in world cells. Passed to `Tanker::spawn`.
    pub start:      UVec3,
    /// Unit XZ direction along the flight axis.
    pub dir:        (f32, f32),
    /// Strip length in cells (varies per kind).
    pub length:     u32,
    /// Strip half-width perpendicular to `dir` (varies per kind).
    pub half_width: i32,
    pub kind:       TankerKind,
    pub delay:      u32,
}

impl TankerRequest {
    /// World cell where the queue badge floats — the midpoint of the
    /// strip, so the marker sits in the middle of whatever the
    /// player will eventually see painted on the ground.
    pub fn badge_cell(&self) -> UVec3 {
        let half = self.length as f32 * 0.5;
        let cx = (self.start.x as f32 + self.dir.0 * half) as u32;
        let cz = (self.start.z as f32 + self.dir.1 * half) as u32;
        UVec3::new(cx, terrain_height(cx, cz), cz)
    }
}

pub(crate) struct Roster {
    pub helis:   [Option<Helicopter>; MAX_HELIS],
    pub crews:   [Option<GroundCrew>; MAX_CREWS],
    pub tankers: [Option<Tanker>;     MAX_TANKERS],
    pub tanker_requests: [Option<TankerRequest>; TANKER_REQUEST_CAP],
    pub hotshots:    [Option<crate::hotshot::HotShot>;    MAX_HOTSHOTS],
    pub drop_planes: [Option<crate::hotshot::DropPlane>;  MAX_DROP_PLANES],
    pub parachutes:  [Option<crate::hotshot::Parachute>;  MAX_PARACHUTES],
    /// Maximum live hot-shot crews permitted by the current scenario
    /// tier. The queue only drains while `alive_hotshots() < hotshot_cap`.
    pub hotshot_cap: u8,
    /// Cell hot-shots walk back to when they time out the pickup
    /// wait. Captured from the helipad on init.
    pub hotshot_home: (u32, u32),
    pub queue:   CommandQueue,
}

impl Roster {
    pub(crate) fn init(heli_count: u8, crew_count: u8, hotshot_count: u8) -> Self {
        let mut helis: [Option<Helicopter>; MAX_HELIS] = [None, None, None, None];
        let mut crews: [Option<GroundCrew>; MAX_CREWS] =
            [None, None, None, None, None, None];

        let helis_to_spawn = (heli_count as usize).min(MAX_HELIS);
        for i in 0..helis_to_spawn {
            // Stagger helis along +X from the pad so their volumes
            // don't visually overlap. 6 cells spacing > 5-wide heli.
            let pad_x = HELI_PAD_X + (i as u32) * 6;
            helis[i] = Some(Helicopter::init(pad_x, HELI_PAD_Z));
        }
        let crews_to_spawn = (crew_count as usize).min(MAX_CREWS);
        for i in 0..crews_to_spawn {
            // Crews line up east of the pad along the road; 4-cell
            // spacing leaves a 1-cell gap between the 3-wide trucks.
            let spawn_x = HELI_PAD_X + 8 + (i as u32) * 4;
            crews[i] = Some(GroundCrew::init(spawn_x, HELI_PAD_Z));
        }
        Self {
            helis,
            crews,
            tankers: [None, None, None, None],
            tanker_requests: [None; TANKER_REQUEST_CAP],
            hotshots:    [None, None, None, None, None, None, None, None],
            drop_planes: [None, None],
            parachutes:  [None, None, None, None],
            hotshot_cap: (hotshot_count as usize).min(MAX_HOTSHOTS) as u8,
            hotshot_home: (HELI_PAD_X, HELI_PAD_Z),
            queue: CommandQueue::new(),
        }
    }

    /// Hand out queued orders this tick, then tick every unit slot.
    /// In-flight orders run to completion regardless of what arrives
    /// in the queue (no-cancel rule). Idle crews with nothing to do
    /// are sent back to base so trucks come to rest at the pad rather
    /// than wherever the last firebreak ended.
    pub(crate) fn tick(&mut self) {
        for slot in self.helis.iter_mut() {
            if let Some(h) = slot {
                if h.is_idle() {
                    if let Some(cell) = self.queue.pop_water() {
                        h.issue_drop(cell);
                    }
                }
            }
        }
        for slot in self.crews.iter_mut() {
            if let Some(c) = slot {
                // Available = Idle OR ReturningHome — a truck on the
                // way back can be preempted with a fresh order.
                if c.is_available() {
                    if let Some(line) = self.queue.pop_line() {
                        let path = line.as_slice();
                        c.issue_path(&path[..line.count as usize]);
                    } else {
                        // Nothing queued — head home if not there yet.
                        c.send_home_if_needed();
                    }
                }
            }
        }
        // Tanker requests run before tanker spawns so a queued
        // sortie can pop into a slot freed earlier this tick.
        self.process_tanker_queue();

        // Drain hotshot queue while there's capacity AND a free
        // drop-plane slot. Each order takes a full drop-plane sortie.
        self.process_hotshot_queue();

        for slot in self.helis.iter_mut() { if let Some(h) = slot { h.tick(); } }
        for slot in self.crews.iter_mut() { if let Some(c) = slot { c.tick(); } }
        for slot in self.tankers.iter_mut() {
            let despawn = match slot.as_mut() {
                Some(t) => !t.tick(),
                None => false,
            };
            if despawn {
                if let Some(t) = slot.take() { t.despawn_actor(); }
            }
        }

        // Drop planes — tick, spawn parachute on drop, despawn off-map.
        self.tick_drop_planes();
        // Parachutes — tick, spawn hot-shot on landing.
        self.tick_parachutes();
        // Hot-shots — tick, despawn any in Done state.
        self.tick_hotshots();

        // Pickup arrival sweep — any heli that latched a
        // `pickup_arrived_slot` this frame has reached its target
        // cell; despawn the matching hot-shot crew (or just clear
        // the flag if the crew already died).
        self.process_pickup_arrivals();

        // Pickup assignment — for each idle heli, find the first
        // hot-shot in AwaitingPickup and dispatch the heli to extract.
        self.process_pickup_dispatch();
    }

    fn process_hotshot_queue(&mut self) {
        loop {
            let squad = crate::hotshot::SQUAD_SIZE as usize;
            // Need SQUAD_SIZE free crew slots (the whole squad has
            // to fit) AND the tier-permitted crew cap has to leave
            // room for another full squad after the drops land.
            let in_flight = self.alive_hotshots()
                + self.parachutes.iter().filter(|s| s.is_some()).count();
            if in_flight + squad > self.hotshot_cap as usize { break; }
            if MAX_HOTSHOTS - self.alive_hotshots() < squad { break; }
            let Some(plane_slot) = self.first_free_drop_plane() else { break; };
            let Some(order) = self.queue.pop_hotshot() else { break; };
            let target = match order.points[0] {
                Some(p) => p,
                None    => continue,
            };
            // Convert FireLinePath → Option-array for each crew's
            // walking path. (Extra slots stay None.)
            let mut path: [Option<(u32, u32)>; CREW_PATH_CAP] = [None; CREW_PATH_CAP];
            for i in 0..(order.count as usize).min(CREW_PATH_CAP) {
                if let Some(p) = order.points[i] {
                    path[i] = Some((p.x, p.z));
                }
            }
            let plane = crate::hotshot::DropPlane::spawn(target, path, self.hotshot_home);
            self.drop_planes[plane_slot] = Some(plane);
        }
    }

    fn first_free_drop_plane(&self) -> Option<usize> {
        for i in 0..MAX_DROP_PLANES {
            if self.drop_planes[i].is_none() { return Some(i); }
        }
        None
    }

    fn first_free_parachute(&self) -> Option<usize> {
        for i in 0..MAX_PARACHUTES {
            if self.parachutes[i].is_none() { return Some(i); }
        }
        None
    }

    fn first_free_hotshot(&self) -> Option<usize> {
        for i in 0..MAX_HOTSHOTS {
            if self.hotshots[i].is_none() { return Some(i); }
        }
        None
    }

    fn alive_hotshots(&self) -> usize {
        self.hotshots.iter().filter(|s| s.is_some()).count()
    }

    fn tick_drop_planes(&mut self) {
        for i in 0..MAX_DROP_PLANES {
            // Tick + capture drop event in one shot.
            let (alive, drop_cell, drop_alt, path, home) = match self.drop_planes[i].as_mut() {
                Some(p) => {
                    let (alive, drop_cell) = p.tick();
                    (alive, drop_cell, p.drop_altitude(), p.path, p.home)
                }
                None => continue,
            };
            if let Some(target) = drop_cell {
                let landing = crate::hotshot::scattered_landing(target);
                if let Some(slot) = self.first_free_parachute() {
                    self.parachutes[slot] = Some(crate::hotshot::Parachute::spawn(
                        landing, drop_alt, path, home,
                    ));
                }
                // If no parachute slot is free, the plane still drops
                // (the moment passes) but no crew lands. Should be
                // rare given MAX_PARACHUTES == MAX_DROP_PLANES.
            }
            if !alive {
                if let Some(p) = self.drop_planes[i].take() { p.despawn_actor(); }
            }
        }
    }

    fn tick_parachutes(&mut self) {
        for i in 0..MAX_PARACHUTES {
            let landed = match self.parachutes[i].as_mut() {
                Some(p) => p.tick(),
                None => continue,
            };
            let Some(landing_cell) = landed else { continue; };
            let (path, home) = {
                let p = self.parachutes[i].as_ref().unwrap();
                (p.path, p.home)
            };
            if let Some(p) = self.parachutes[i].take() { p.despawn_actor(); }
            if let Some(slot) = self.first_free_hotshot() {
                self.hotshots[slot] = Some(crate::hotshot::HotShot::spawn(
                    landing_cell, home, path,
                ));
            }
        }
    }

    fn tick_hotshots(&mut self) {
        for slot in self.hotshots.iter_mut() {
            if let Some(h) = slot { h.tick(); }
        }
        // Sweep Done crews.
        for slot in self.hotshots.iter_mut() {
            let drop = match slot.as_ref() {
                Some(h) => h.is_done(),
                None => false,
            };
            if drop {
                if let Some(h) = slot.take() { h.despawn_actor(); }
            }
        }
    }

    fn process_pickup_arrivals(&mut self) {
        for hi in 0..MAX_HELIS {
            let pickup_slot = self.helis[hi].as_ref().and_then(|h| h.pickup_arrived_slot());
            let Some(hs_slot) = pickup_slot else { continue; };
            if let Some(h) = self.helis[hi].as_mut() {
                h.clear_pickup_arrived();
            }
            if let Some(hs) = self.hotshots[hs_slot as usize].as_mut() {
                hs.mark_extracted();
            }
        }
    }

    fn process_pickup_dispatch(&mut self) {
        for hi in 0..MAX_HELIS {
            let idle = self.helis[hi].as_ref().map(|h| h.is_idle()).unwrap_or(false);
            if !idle { continue; }
            let mut chosen: Option<u8> = None;
            for hs_i in 0..MAX_HOTSHOTS {
                if let Some(hs) = self.hotshots[hs_i].as_ref() {
                    if hs.is_awaiting_pickup() {
                        chosen = Some(hs_i as u8);
                        break;
                    }
                }
            }
            let Some(hs_slot) = chosen else { continue; };
            let (cx, cz) = self.hotshots[hs_slot as usize]
                .as_ref()
                .unwrap()
                .cell();
            if let Some(h) = self.helis[hi].as_mut() {
                h.issue_pickup(hs_slot, (cx as f32 + 0.5, cz as f32 + 0.5));
            }
            if let Some(hs) = self.hotshots[hs_slot as usize].as_mut() {
                hs.mark_being_picked();
            }
        }
    }

    pub(crate) fn dispatch_water_drop(&mut self, cell: UVec3) {
        if self.queue.push_water(cell) {
            // Paint a single cyan voxel floating above the target so
            // the player can see what's pending at a glance, alongside
            // the queue badge. The voxel is cleared by `drop_water`
            // when the helicopter actually drops on the cell.
            let marker = UVec3::new(cell.x, cell.y + PLANNED_WATER_Y_OFFSET, cell.z);
            if physics::material_at(marker.x, marker.y, marker.z) == 0 {
                set_voxel(marker, M_PLANNED_WATER);
            }
        }
    }

    /// Queue a water-tanker sortie aimed at `cell`. The plane flies
    /// due north and the drop strip is centred on `cell` — `start`
    /// is offset south by half the strip length so the painted line
    /// straddles the click. Returns `true` iff the request was
    /// queued.
    pub(crate) fn dispatch_water_tanker(&mut self, cell: UVec3) -> bool {
        let length: u32 = 25;
        let half_z = length / 2;
        let start = UVec3::new(cell.x, cell.y, cell.z.saturating_sub(half_z));
        self.enqueue_tanker(TankerRequest {
            start,
            dir:        (0.0, 1.0),
            length,
            half_width: 2,
            kind:       TankerKind::Water,
            delay:      TANKER_DELAY_TICKS,
        })
    }

    /// Queue a retardant sortie along the player's aimed line.
    /// `start` is the strip's start point (the wheel anchor); `dir`
    /// is the unit XZ direction. Returns `true` iff the request was
    /// queued. The actual tanker spawns later — after
    /// `TANKER_DELAY_TICKS` — once a tanker slot is free; see
    /// `process_tanker_queue`.
    pub(crate) fn dispatch_retardant_strip(
        &mut self,
        start: UVec3,
        dir: (f32, f32),
    ) -> bool {
        self.enqueue_tanker(TankerRequest {
            start,
            dir,
            length:     crate::retardant_aim::RETARDANT_LENGTH,
            half_width: 1,
            kind:       TankerKind::Retardant,
            delay:      TANKER_DELAY_TICKS,
        })
    }

    fn enqueue_tanker(&mut self, req: TankerRequest) -> bool {
        for slot in self.tanker_requests.iter_mut() {
            if slot.is_none() {
                *slot = Some(req);
                return true;
            }
        }
        false
    }

    /// Per-tick processing of the shared tanker request queue.
    /// Decrements the head's delay; when the head's delay reaches 0
    /// and a tanker slot is available, spawns a real `Tanker` and
    /// shifts the queue forward. Only the head ticks down per frame
    /// so queue position 1 reaches the airspace before 2 does.
    fn process_tanker_queue(&mut self) {
        let head_snapshot = match self.tanker_requests[0].as_ref() {
            Some(r) => Some(*r),
            None => None,
        };
        let head = match head_snapshot {
            Some(x) => x,
            None => return,
        };
        if head.delay > 0 {
            if let Some(r) = self.tanker_requests[0].as_mut() {
                r.delay = head.delay - 1;
            }
            return;
        }
        // Delay's up — try to spawn. If the actor pool is exhausted
        // the request stays in the queue and we'll try again next
        // tick.
        let spawned = self.spawn_tanker(
            head.start,
            head.dir,
            head.length,
            head.half_width,
            head.kind,
        );
        if spawned {
            // Shift the queue forward so the next request becomes
            // the new head (and gets badge position 1 next frame).
            for i in 0..TANKER_REQUEST_CAP - 1 {
                self.tanker_requests[i] = self.tanker_requests[i + 1].take();
            }
        }
    }

    fn spawn_tanker(
        &mut self,
        start: UVec3,
        dir: (f32, f32),
        length: u32,
        half_width: i32,
        kind: TankerKind,
    ) -> bool {
        for slot in self.tankers.iter_mut() {
            if slot.is_none() {
                *slot = Some(Tanker::spawn(start, dir, length, half_width, kind));
                return true;
            }
        }
        false
    }

    pub(crate) fn dispatch_fire_line(&mut self, points: &[UVec3]) {
        if points.is_empty() { return; }
        self.queue.push_line(FireLinePath::from_slice(points));
    }

    /// Queue a hot-shot deployment along `points`. The crew
    /// parachutes in at the first waypoint, lays firebreak along
    /// the polyline, then awaits a heli pickup. Returns `true` iff
    /// the order was queued (the queue's HOTSHOT_QUEUE_CAP guards
    /// the array; orders past that are silently dropped). Also a
    /// no-op when the scenario tier provides 0 hot-shot capacity —
    /// the cart can still draft, but the order won't deploy.
    pub(crate) fn dispatch_hotshot_line(&mut self, points: &[UVec3]) -> bool {
        if points.is_empty() || self.hotshot_cap == 0 { return false; }
        self.queue.push_hotshot(FireLinePath::from_slice(points))
    }

    // ── Pool-state accessors for HUD ─────────────────────────────

    pub(crate) fn heli_total(&self) -> u32 {
        self.helis.iter().filter(|s| s.is_some()).count() as u32
    }
    pub(crate) fn heli_busy(&self) -> u32 {
        self.helis.iter().filter_map(|s| s.as_ref())
            .filter(|h| !h.is_idle()).count() as u32
    }
    pub(crate) fn crew_total(&self) -> u32 {
        self.crews.iter().filter(|s| s.is_some()).count() as u32
    }
    pub(crate) fn crew_busy(&self) -> u32 {
        self.crews.iter().filter_map(|s| s.as_ref())
            .filter(|c| !c.is_idle()).count() as u32
    }
    pub(crate) fn hotshot_total(&self) -> u32 { self.hotshot_cap as u32 }
    pub(crate) fn hotshot_busy(&self) -> u32 {
        self.hotshots.iter().filter(|s| s.is_some()).count() as u32
    }
}

#[allow(dead_code)]
fn _crew_size_hint() -> u8 { CREW_SIZE_Y }
