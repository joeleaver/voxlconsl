//! hello-cube — a first voxlconsl cart with a controllable character actor.
//!
//! Builds a chequered ground, a voxel tree with leaf canopy, a ruby on top,
//! and a few gold cubes scattered around. Spawns a "little dude" actor
//! the player drives with WASD; the camera orbits around the dude and
//! follows him as he moves.

#![no_std]
#![no_main]

use voxlconsl_sdk::*;

const CHUNK_SIDE: u32 = 32;
const CHUNK_CENTER: f32 = 16.0;

const M_STONE: u8 = 1;
const M_WOOD:  u8 = 2;
const M_LEAF:  u8 = 3;
const M_RUBY:  u8 = 4;
const M_GOLD:  u8 = 5;
const M_GRASS: u8 = 6;
const M_SKIN:  u8 = 7;
const M_SHIRT: u8 = 8;

// Camera state — orbit around the dude.
static mut CAM_YAW: f32 = 0.7;
static mut CAM_PITCH: f32 = 0.5;
static mut CAM_DISTANCE: f32 = 14.0;

// The player's actor.
static mut PLAYER: Option<ActorId> = None;
static mut PLAYER_POS: Vec3 = Vec3 { x: 16.0, y: 1.0, z: 16.0 };
static mut PLAYER_FACING: f32 = 0.0;

// Action handles.
static mut MOVE_ACTION: ActionHandle = ActionHandle(0);
static mut AIM_ACTION: ActionHandle = ActionHandle(0);
static mut FIRE_ACTION: ActionHandle = ActionHandle(0);

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    // ── Materials ─────────────────────────────────────────────
    material_define(M_STONE, Material::pack_color(14, 1), 0, MaterialFlags::empty());
    material_define(M_WOOD,  Material::pack_color( 0, 1), 0, MaterialFlags::empty());
    material_define(M_LEAF,  Material::pack_color( 2, 2), 0, MaterialFlags::empty());
    material_define(
        M_RUBY,
        Material::pack_color(10, 2),
        6,
        MaterialFlags::empty().with(MaterialFlags::GLOSSY),
    );
    material_define(
        M_GOLD,
        Material::pack_color(12, 3),
        0,
        MaterialFlags::empty().with(MaterialFlags::GLOSSY),
    );
    material_define(M_GRASS, Material::pack_color(3, 2), 0, MaterialFlags::empty());
    material_define(M_SKIN,  Material::pack_color(1, 3), 0, MaterialFlags::empty());
    material_define(M_SHIRT, Material::pack_color(7, 2), 0, MaterialFlags::empty());

    // ── Sky + sun ─────────────────────────────────────────────
    sky_set_gradient(Material::pack_color(7, 0), Material::pack_color(6, 0));
    light_set_sun(Vec3::new(-0.6, 0.8, 0.4), 0, 0);

    // ── World geometry ────────────────────────────────────────
    for x in 0..CHUNK_SIDE {
        for z in 0..CHUNK_SIDE {
            let m = if (x + z) % 2 == 0 { M_STONE } else { M_GRASS };
            set_voxel(UVec3::new(x, 0, z), m);
        }
    }

    let cx: u32 = 8;
    let cz: u32 = 24;
    for y in 1..6 { set_voxel(UVec3::new(cx, y, cz), M_WOOD); }
    for dy in 0..3 {
        let r: i32 = if dy == 1 { 2 } else { 1 };
        let y = 6 + dy as u32;
        for dx in -r..=r {
            for dz in -r..=r {
                if dx * dx + dz * dz <= r * r + 1 {
                    let x = (cx as i32 + dx) as u32;
                    let z = (cz as i32 + dz) as u32;
                    set_voxel(UVec3::new(x, y, z), M_LEAF);
                }
            }
        }
    }
    set_voxel(UVec3::new(cx, 9, cz), M_RUBY);

    for &(x, z) in &[(4u32, 4), (28, 6), (5, 27), (26, 26), (12, 22)] {
        set_voxel(UVec3::new(x, 1, z), M_GOLD);
    }

    // ── Player actor ──────────────────────────────────────────
    // A 5×8×3 little dude: brown legs, blue shirt, skin head.
    let id = actor_spawn().expect("failed to spawn player actor");
    unsafe { PLAYER = Some(id); }

    // Legs (y 0..2)
    actor_fill_box(id, U8Vec3::new(1, 0, 1), U8Vec3::new(1, 1, 1), M_WOOD);
    actor_fill_box(id, U8Vec3::new(3, 0, 1), U8Vec3::new(3, 1, 1), M_WOOD);
    // Torso (y 2..5), centered on x=2
    actor_fill_box(id, U8Vec3::new(1, 2, 0), U8Vec3::new(3, 4, 2), M_SHIRT);
    // Arms (y 2..4), x = 0 and x = 4
    actor_fill_box(id, U8Vec3::new(0, 2, 1), U8Vec3::new(0, 3, 1), M_SHIRT);
    actor_fill_box(id, U8Vec3::new(4, 2, 1), U8Vec3::new(4, 3, 1), M_SHIRT);
    // Head (y 5..7)
    actor_fill_box(id, U8Vec3::new(1, 5, 0), U8Vec3::new(3, 6, 2), M_SKIN);

    // Drop the dude in the middle of the world, raised so feet sit on the
    // ground (world ground = y=0 surface, so dude's local y=0 sits at y=1).
    unsafe { actor_set_position(id, PLAYER_POS); }

    // ── Input actions ─────────────────────────────────────────
    unsafe {
        MOVE_ACTION = input_declare_action(ActionKind::Axis2D, BindingHint::PrimaryMovement, "move");
        AIM_ACTION  = input_declare_action(ActionKind::Axis2D, BindingHint::Aim, "aim");
        FIRE_ACTION = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire, "fire");
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    let dt = (dt_ms as f32) / 1000.0;
    let (mx, my) = input_action_axis2d(unsafe { MOVE_ACTION });
    let (ax, ay) = input_action_axis2d(unsafe { AIM_ACTION });

    // Camera mouse aim feeds yaw/pitch.
    unsafe {
        CAM_YAW += ax * 0.005;
        CAM_PITCH += -ay * 0.005;
        CAM_PITCH = CAM_PITCH.clamp(-1.2, 1.2);
    }

    // WASD drives the dude relative to camera-facing direction.
    let move_speed = 6.0_f32;
    let cam_yaw = unsafe { CAM_YAW };
    let forward = Vec3::new(sine(cam_yaw), 0.0, cosine(cam_yaw));
    let right = Vec3::new(cosine(cam_yaw), 0.0, -sine(cam_yaw));

    let movement = Vec3::new(
        right.x * mx + forward.x * my,
        0.0,
        right.z * mx + forward.z * my,
    );
    let speed_sq = movement.x * movement.x + movement.z * movement.z;

    if let Some(player) = unsafe { PLAYER } {
        unsafe {
            PLAYER_POS.x = (PLAYER_POS.x + movement.x * move_speed * dt).clamp(0.0, CHUNK_SIDE as f32 - 5.0);
            PLAYER_POS.z = (PLAYER_POS.z + movement.z * move_speed * dt).clamp(0.0, CHUNK_SIDE as f32 - 3.0);
            actor_set_position(player, PLAYER_POS);

            // Face the direction of movement when walking.
            if speed_sq > 0.0025 {
                PLAYER_FACING = -atan2(movement.x, movement.z);
                actor_set_yaw(player, PLAYER_FACING);
            }
        }

        // Edge-detected FIRE cycles the player's shirt color through ramps.
        if input_action_pressed(unsafe { FIRE_ACTION }) {
            static mut SHIRT_RAMP: u8 = 7;
            unsafe {
                SHIRT_RAMP = (SHIRT_RAMP + 1) & 0x0F;
                material_define(M_SHIRT, Material::pack_color(SHIRT_RAMP, 2), 0, MaterialFlags::empty());
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    let (yaw, pitch, dist) = unsafe { (CAM_YAW, CAM_PITCH, CAM_DISTANCE) };
    let pos = unsafe { PLAYER_POS };

    // Orbit around the player, eye at distance/pitch from the player's chest height.
    let cos_pitch = cosine(pitch);
    let target = Vec3::new(pos.x + 2.5, pos.y + 4.0, pos.z + 1.5);
    let eye = Vec3::new(
        target.x + dist * sine(yaw) * cos_pitch,
        target.y + dist * sine(pitch),
        target.z + dist * cosine(yaw) * cos_pitch,
    );

    let _ = CHUNK_CENTER;  // kept for backward source compat; unused now
    camera_set_lookat(eye, target, Vec3::Y);
    camera_set_fov(60.0);
}

// ── tiny no_std math (good to ~0.001 in [-pi, pi]) ─────────────────────────

fn sine(x: f32) -> f32 {
    let two_pi = core::f32::consts::TAU;
    let mut x = x % two_pi;
    if x > core::f32::consts::PI { x -= two_pi; }
    if x < -core::f32::consts::PI { x += two_pi; }
    let x2 = x * x;
    x * (1.0 - x2 * (1.0 / 6.0 - x2 * (1.0 / 120.0 - x2 / 5040.0)))
}
fn cosine(x: f32) -> f32 { sine(x + core::f32::consts::FRAC_PI_2) }

/// `atan2` to ~0.01 rad accuracy. Sufficient for character facing.
fn atan2(y: f32, x: f32) -> f32 {
    if x == 0.0 && y == 0.0 { return 0.0; }
    let abs_x = if x < 0.0 { -x } else { x };
    let abs_y = if y < 0.0 { -y } else { y };
    let (a, swapped) = if abs_x > abs_y {
        (abs_y / abs_x, false)
    } else {
        (abs_x / abs_y, true)
    };
    // Rational approx of atan(a) for a in [0, 1].
    let r = a * (0.97 - 0.19 * a * a);
    let r = if swapped { core::f32::consts::FRAC_PI_2 - r } else { r };
    let r = if x < 0.0 { core::f32::consts::PI - r } else { r };
    if y < 0.0 { -r } else { r }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log("cart panicked");
    let _ = info;
    loop {}
}
