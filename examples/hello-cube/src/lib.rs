//! hello-cube — a first voxlconsl cart with player input.
//!
//! Builds a chequered ground, a voxel tree with leaf canopy, a ruby on top,
//! and a few gold cubes scattered around. Player drives a free-look orbit
//! camera with WASD (orbit around scene + dolly in/out) and the mouse
//! (aim/pitch).

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

// Camera state — driven by input each frame. Initial values pick a nice
// 3/4 orbit angle so the first rendered frame already shows the scene
// well (instead of looking dead-on at the tree's face from yaw = 0).
static mut CAM_YAW: f32 = 0.7;          // around world Y
static mut CAM_PITCH: f32 = 0.5;        // looking slightly down
static mut CAM_DISTANCE: f32 = 26.0;    // orbit radius

// Action handles — populated in init(), read in update/render.
static mut MOVE_ACTION: ActionHandle = ActionHandle(0);
static mut AIM_ACTION: ActionHandle = ActionHandle(0);
static mut FIRE_ACTION: ActionHandle = ActionHandle(0);

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    // Materials.
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

    // Sky + sun.
    sky_set_gradient(
        Material::pack_color(7, 0), // deep blue top
        Material::pack_color(6, 0), // sky blue horizon
    );
    light_set_sun(Vec3::new(-0.6, 0.8, 0.4), 0, 0);

    // Chequered ground.
    for x in 0..CHUNK_SIDE {
        for z in 0..CHUNK_SIDE {
            let m = if (x + z) % 2 == 0 { M_STONE } else { M_GRASS };
            set_voxel(UVec3::new(x, 0, z), m);
        }
    }

    // Tree trunk.
    let cx: u32 = 16;
    let cz: u32 = 16;
    for y in 1..6 {
        set_voxel(UVec3::new(cx, y, cz), M_WOOD);
    }

    // Leaves.
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

    // Ruby crown.
    set_voxel(UVec3::new(cx, 9, cz), M_RUBY);

    // Gold cubes.
    for &(x, z) in &[(4u32, 4), (28, 6), (5, 27), (26, 26), (12, 22)] {
        set_voxel(UVec3::new(x, 1, z), M_GOLD);
    }

    // Declare the input actions this cart cares about.
    unsafe {
        MOVE_ACTION = input_declare_action(ActionKind::Axis2D, BindingHint::PrimaryMovement, "move");
        AIM_ACTION  = input_declare_action(ActionKind::Axis2D, BindingHint::Aim, "aim");
        FIRE_ACTION = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire, "fire");
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    let dt = (dt_ms as f32) / 1000.0;

    // WASD: forward/back dollies in/out, left/right orbit yaw.
    let (mx, my) = input_action_axis2d(unsafe { MOVE_ACTION });
    let dolly_speed = 18.0_f32;     // voxels/sec
    let orbit_speed = 2.0_f32;      // rad/sec

    // Mouse: aim. Y inverted so dragging up looks up.
    let (ax, ay) = input_action_axis2d(unsafe { AIM_ACTION });
    let mouse_yaw   = ax * 0.005;   // rad per pixel
    let mouse_pitch = -ay * 0.005;

    unsafe {
        CAM_YAW += mx * orbit_speed * dt + mouse_yaw;
        CAM_PITCH += mouse_pitch;
        CAM_PITCH = CAM_PITCH.clamp(-1.2, 1.2);
        CAM_DISTANCE -= my * dolly_speed * dt;
        CAM_DISTANCE = CAM_DISTANCE.clamp(8.0, 80.0);

        // Tap PrimaryFire to "lock" the ruby material to a different shade
        // each press — proves edge-detection works end-to-end.
        if input_action_pressed(FIRE_ACTION) {
            static mut RUBY_SHADE: u8 = 2;
            RUBY_SHADE = if RUBY_SHADE == 3 { 0 } else { RUBY_SHADE + 1 };
            material_define(
                M_RUBY,
                Material::pack_color(10, RUBY_SHADE),
                if RUBY_SHADE == 3 { 14 } else { 6 },
                MaterialFlags::empty().with(MaterialFlags::GLOSSY),
            );
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    let (yaw, pitch, dist) = unsafe { (CAM_YAW, CAM_PITCH, CAM_DISTANCE) };

    // Spherical-coordinate orbit camera around scene centre.
    let target = Vec3::new(CHUNK_CENTER, 6.0, CHUNK_CENTER);
    let cos_pitch = cosine(pitch);
    let eye = Vec3::new(
        target.x + dist * sine(yaw) * cos_pitch,
        target.y + dist * sine(pitch),
        target.z + dist * cosine(yaw) * cos_pitch,
    );

    camera_set_lookat(eye, target, Vec3::Y);
    camera_set_fov(60.0);
}

// ── tiny no_std trig (good to ~0.001 in [-pi, pi]) ────────────────────────

fn sine(x: f32) -> f32 {
    let two_pi = core::f32::consts::TAU;
    let mut x = x % two_pi;
    if x > core::f32::consts::PI { x -= two_pi; }
    if x < -core::f32::consts::PI { x += two_pi; }
    let x2 = x * x;
    x * (1.0 - x2 * (1.0 / 6.0 - x2 * (1.0 / 120.0 - x2 / 5040.0)))
}
fn cosine(x: f32) -> f32 { sine(x + core::f32::consts::FRAC_PI_2) }

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log("cart panicked");
    let _ = info;
    loop {}
}
