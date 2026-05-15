//! The little dude — 5×7×3 prefab, flipbook walk cycle, terrain-
//! tracking movement.
//!
//! Four prefab frames: one idle pose + three walk frames whose legs
//! and arms swing in counterphase. The `Flipbook` helper cycles
//! frames at 140 ms/frame whenever the player is moving; stopping
//! snaps back to the idle prefab.
//!
//! Yaw rotates the prefab around its volume's horizontal centre so
//! the actor visually stays anchored to the player's `(x, z)` no
//! matter which way they face — the cart doesn't need facing-
//! dependent offset math.

use voxlconsl_sdk::*;
use voxlconsl_sdk::animation::Flipbook;

use crate::{M_SHIRT, M_SKIN, M_WOOD, WORLD};
use crate::mathlib;
use crate::terrain::terrain_height;

// ── Prefab geometry ──────────────────────────────────────────────

pub(crate) const DUDE_W: usize = 5;
pub(crate) const DUDE_H: usize = 7;
pub(crate) const DUDE_D: usize = 3;
pub(crate) const DUDE_VOL: usize = DUDE_W * DUDE_H * DUDE_D;

const P_IDLE:   PrefabId = PrefabId(1);
const P_WALK_0: PrefabId = PrefabId(2);
const P_WALK_1: PrefabId = PrefabId(3);
const P_WALK_2: PrefabId = PrefabId(4);

static mut DENSE_IDLE:   [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_WALK_0: [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_WALK_1: [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_WALK_2: [u8; DUDE_VOL] = [0; DUDE_VOL];

const WALK_FRAMES: &[PrefabId] = &[P_WALK_0, P_WALK_1, P_WALK_2, P_WALK_1];
static mut WALK_FB: Flipbook = Flipbook::new(WALK_FRAMES, 140, true);
static mut CURRENT_FRAME: PrefabId = P_IDLE;

// ── State ─────────────────────────────────────────────────────────

pub(crate) static mut PLAYER:        Option<ActorId> = None;
pub(crate) static mut PLAYER_POS:    Vec3 = Vec3 { x: 256.0, y: 32.0, z: 256.0 };
pub(crate) static mut PLAYER_FACING: f32 = 0.0;

// ── Boot ──────────────────────────────────────────────────────────

/// Bake the four prefab frames, spawn the player actor at the centre
/// of the map (sampled from the heightmap), and keep it hidden until
/// FIRE pulls the camera into the gameplay scene.
pub(crate) fn init() {
    unsafe {
        // IDLE: legs straight (z=1), arms at sides (z=1).
        // WALK frames: feet/arms swing in counterphase so the cycle reads.
        build_dude(&mut *(&raw mut DENSE_IDLE),   1, 1, 1, 1);
        build_dude(&mut *(&raw mut DENSE_WALK_0), 0, 2, 2, 0);
        build_dude(&mut *(&raw mut DENSE_WALK_1), 1, 1, 1, 1);
        build_dude(&mut *(&raw mut DENSE_WALK_2), 2, 0, 0, 2);

        let size = U8Vec3::new(DUDE_W as u8, DUDE_H as u8, DUDE_D as u8);
        prefab_define(P_IDLE,   &*(&raw const DENSE_IDLE),   size);
        prefab_define(P_WALK_0, &*(&raw const DENSE_WALK_0), size);
        prefab_define(P_WALK_1, &*(&raw const DENSE_WALK_1), size);
        prefab_define(P_WALK_2, &*(&raw const DENSE_WALK_2), size);

        let id = actor_spawn_from(P_IDLE, Orientation::Up).expect("player");
        // Drop the player on the surface at the centre.
        let h = terrain_height(256, 256);
        PLAYER_POS = Vec3::new(254.0, h as f32, 254.0);
        PLAYER = Some(id);
        actor_set_position(id, PLAYER_POS);
        CURRENT_FRAME = P_IDLE;
        // Hide until FIRE — actors are cart-global and we don't want
        // the dude in the title scene's frame.
        actor_set_visible(id, false);
    }
}

/// Reveal the player. Called once when transitioning from title →
/// gameplay scene.
pub(crate) fn make_visible() {
    unsafe {
        if let Some(p) = PLAYER { actor_set_visible(p, true); }
    }
}

// ── Per-frame movement + walk cycle ──────────────────────────────

/// Move the player by `mx, my` (axis2d reading) relative to the
/// camera's yaw, sample the heightmap for surface tracking, and
/// advance the walk-cycle flipbook. Called from `update()`.
pub(crate) fn tick(mx: f32, my: f32, cam_yaw: f32, dt: f32, dt_ms: u32) {
    // forward = where the camera is *looking* (toward target), in the
    // ground plane only. Vertical look doesn't affect movement, so the
    // dude moves predictably even when the camera is steeply angled.
    let forward = Vec3::new(-mathlib::sine(cam_yaw), 0.0, -mathlib::cosine(cam_yaw));
    let right   = Vec3::new( mathlib::cosine(cam_yaw), 0.0, -mathlib::sine(cam_yaw));
    let movement = Vec3::new(
        right.x * mx + forward.x * my,
        0.0,
        right.z * mx + forward.z * my,
    );
    let speed = 12.0_f32;
    let speed_sq = movement.x * movement.x + movement.z * movement.z;

    if let Some(player) = unsafe { PLAYER } {
        unsafe {
            let moving = speed_sq > 0.0025;

            PLAYER_POS.x = (PLAYER_POS.x + movement.x * speed * dt).clamp(2.0, (WORLD - 7) as f32);
            PLAYER_POS.z = (PLAYER_POS.z + movement.z * speed * dt).clamp(2.0, (WORLD - 5) as f32);
            // Sample the heightmap each frame so the dude tracks the terrain.
            let h = terrain_height(PLAYER_POS.x as u32, PLAYER_POS.z as u32);
            PLAYER_POS.y = h as f32;
            actor_set_position(player, PLAYER_POS);
            if moving {
                PLAYER_FACING = -mathlib::atan2(movement.x, movement.z);
                actor_set_yaw(player, PLAYER_FACING);
            }

            // Animate while moving, snap to idle when stopped. Only
            // call set_prefab on transitions — the swap is cheap but
            // spamming it is wasteful.
            let walk_fb = &mut *(&raw mut WALK_FB);
            let want = if moving {
                walk_fb.tick(dt_ms);
                walk_fb.current()
            } else {
                walk_fb.reset();
                P_IDLE
            };
            if want != CURRENT_FRAME {
                actor_set_prefab(player, want);
                CURRENT_FRAME = want;
            }
        }
    }
}

// ── Prefab authoring ─────────────────────────────────────────────

fn idx(x: usize, y: usize, z: usize) -> usize {
    (z * DUDE_H + y) * DUDE_W + x
}

fn put(buf: &mut [u8; DUDE_VOL], x: usize, y: usize, z: usize, m: u8) {
    if x < DUDE_W && y < DUDE_H && z < DUDE_D {
        buf[idx(x, y, z)] = m;
    }
}

/// Build one frame of the little dude into `buf`.
///
/// `left_leg_z` / `right_leg_z` / `arm_l_z` / `arm_r_z` are 0..=2
/// (front/middle/back). Idle uses z=1 for everything; walk frames
/// swing legs and arms in counterphase.
fn build_dude(
    buf: &mut [u8; DUDE_VOL],
    left_leg_z: usize, right_leg_z: usize,
    arm_l_z: usize, arm_r_z: usize,
) {
    *buf = [0; DUDE_VOL];
    // Legs (y=0..=1)
    put(buf, 1, 0, left_leg_z,  M_WOOD); put(buf, 1, 1, left_leg_z,  M_WOOD);
    put(buf, 3, 0, right_leg_z, M_WOOD); put(buf, 3, 1, right_leg_z, M_WOOD);
    // Torso 3×3 (x=1..3, y=2..4, z=1)
    for x in 1..=3 { for y in 2..=4 { put(buf, x, y, 1, M_SHIRT); } }
    // Arms (x=0/4, y=2..3) at the swing offset
    put(buf, 0, 2, arm_l_z, M_SHIRT); put(buf, 0, 3, arm_l_z, M_SHIRT);
    put(buf, 4, 2, arm_r_z, M_SHIRT); put(buf, 4, 3, arm_r_z, M_SHIRT);
    // Head 3×2×3 (x=1..3, y=5..6, full z)
    for x in 1..=3 {
        for y in 5..=6 {
            for z in 0..DUDE_D { put(buf, x, y, z, M_SKIN); }
        }
    }
}
