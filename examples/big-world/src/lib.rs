//! big-world — voxlconsl's renderer stress test.
//!
//! Builds a 512×512 voxel terrain from cart-side multi-octave value
//! noise, sprinkles ~500 trees on grass tiles, and drops the player
//! down at the centre of the map. The whole point is to populate
//! ~hundreds of chunks across the active scene and see the renderer
//! still hold a sensible frame rate.
//!
//! Memory math (see SPEC.md §13.8):
//!   - 512×512 ground × ~10 voxels deep = ~2.6 M voxels populated
//!   - Across 16×16 = 256 X/Z chunks × 1–2 Y chunks = 256–512 chunks
//!   - At ~50 KB/chunk SVO+dense = ~12–25 MB resident
//!
//! That fits the spec's ESP32-P4 design point (≈ 25 MB voxel-data
//! budget). On smaller MCUs this cart is honestly out-of-spec and
//! exists only to flex the renderer.

#![no_std]
#![no_main]

use voxlconsl_sdk::*;
use voxlconsl_sdk::animation::Flipbook;
use voxlconsl_sdk::text::{measure, paint_world, Axis, FONT_ANSI, FONT_DCP1};

const WORLD: u32 = 512;

// ── Scenes ──────────────────────────────────────────────────────────────
// Scene 0 is the title screen the cart boots into; FIRE transitions
// the player into the gameplay world (scene 1).
const SCENE_TITLE: SceneId = SceneId(0);
const SCENE_GAME:  SceneId = SceneId(1);

#[derive(Copy, Clone, PartialEq, Eq)]
enum GameState { Title, Playing }
static mut STATE: GameState = GameState::Title;
static mut TITLE_CLOCK_MS: u32 = 0;

const M_STONE: u8 = 1;
const M_DIRT:  u8 = 2;
const M_GRASS: u8 = 3;
const M_WOOD:  u8 = 4;
const M_LEAF:  u8 = 5;
const M_SKIN:  u8 = 6;
const M_SHIRT: u8 = 7;
const M_SIGN_BODY: u8 = 8;
const M_SIGN_FACE: u8 = 9;

// ── Player ────────────────────────────────────────────────────────────────
const DUDE_W: usize = 5;
const DUDE_H: usize = 7;
const DUDE_D: usize = 3;
const DUDE_VOL: usize = DUDE_W * DUDE_H * DUDE_D;

// Four prefab frames: idle + three walk poses (0/1/2 swing the legs
// and arms in counterphase). Same scheme as hello-cube.
const P_IDLE:   PrefabId = PrefabId(1);
const P_WALK_0: PrefabId = PrefabId(2);
const P_WALK_1: PrefabId = PrefabId(3);
const P_WALK_2: PrefabId = PrefabId(4);

static mut DENSE_IDLE:   [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_WALK_0: [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_WALK_1: [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_WALK_2: [u8; DUDE_VOL] = [0; DUDE_VOL];

static mut PLAYER: Option<ActorId> = None;
static mut PLAYER_POS: Vec3 = Vec3 { x: 256.0, y: 32.0, z: 256.0 };
static mut PLAYER_FACING: f32 = 0.0;

const WALK_FRAMES: &[PrefabId] = &[P_WALK_0, P_WALK_1, P_WALK_2, P_WALK_1];
static mut WALK_FB: Flipbook = Flipbook::new(WALK_FRAMES, 140, true);
static mut CURRENT_FRAME: PrefabId = P_IDLE;

// Camera state — orbit around the dude.
static mut CAM_YAW: f32 = 0.7;
static mut CAM_PITCH: f32 = 0.45;
static mut CAM_DISTANCE: f32 = 28.0;

// Action handles.
static mut MOVE_ACTION: ActionHandle = ActionHandle(0);
static mut AIM_ACTION:  ActionHandle = ActionHandle(0);
static mut FIRE_ACTION: ActionHandle = ActionHandle(0);

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    // ── Materials ─────────────────────────────────────────────
    material_define(M_STONE, Material::pack_color(14, 1), 0, MaterialFlags::empty());
    material_define(M_DIRT,  Material::pack_color( 0, 1), 0, MaterialFlags::empty());
    material_define(M_GRASS, Material::pack_color( 3, 2), 0, MaterialFlags::empty());
    material_define(M_WOOD,  Material::pack_color( 0, 0), 0, MaterialFlags::empty());
    material_define(M_LEAF,  Material::pack_color( 2, 2), 0, MaterialFlags::empty());
    material_define(M_SKIN,  Material::pack_color( 1, 3), 0, MaterialFlags::empty());
    material_define(M_SHIRT, Material::pack_color( 7, 2), 0, MaterialFlags::empty());
    // Sign body = warm dark wood; face = bright emissive accent so the
    // letters glow off the front of the slab.
    material_define(M_SIGN_BODY, Material::pack_color( 0, 0), 0, MaterialFlags::empty());
    material_define(M_SIGN_FACE, Material::pack_color(13, 3), 12, MaterialFlags::empty());

    sky_set_gradient(Material::pack_color(7, 0), Material::pack_color(6, 0));
    light_set_sun(Vec3::new(-0.6, 0.8, 0.4), 0, 0);

    // The cart owns two scenes: a clean void where the title text
    // floats (scene 0) and the gameplay world below (scene 1). We
    // build scene 1 first, then scene 0, leaving 0 active so the cart
    // boots into the title.
    scene_set_active(SCENE_GAME);

    // ── Terrain ───────────────────────────────────────────────
    //
    // For every (x, z) column on the 512×512 grid: sample the noise
    // height, lay stone up to height-3, dirt for height-3..height-1,
    // grass cap on top. fill_box collapses each column into one host
    // call — one set_voxel per voxel would be a half-million extra
    // round-trips during init.
    let mut z = 0u32;
    while z < WORLD {
        let mut x = 0u32;
        while x < WORLD {
            let h = terrain_height(x, z);
            // Stone fill.
            if h > 4 {
                fill_box(UVec3::new(x, 0, z), UVec3::new(x, h - 4, z), M_STONE);
            }
            // Dirt band right under the surface.
            if h >= 2 {
                let dirt_lo = if h > 3 { h - 3 } else { 0 };
                fill_box(UVec3::new(x, dirt_lo, z), UVec3::new(x, h - 2, z), M_DIRT);
            }
            // Grass surface.
            if h > 0 {
                set_voxel(UVec3::new(x, h - 1, z), M_GRASS);
            }
            x += 1;
        }
        z += 1;
    }

    // ── Trees ─────────────────────────────────────────────────
    //
    // Scatter ~500 trees with an LCG-derived placement so they're
    // deterministic. plant_tree samples the heightmap to anchor at
    // the surface.
    let mut prng = 0xDEAD_BEEFu32;
    let mut planted = 0u32;
    while planted < 500 {
        prng = prng.wrapping_mul(0x9E37_79B9).wrapping_add(0x1234_5678);
        // Canopy spans cx±3, cz±3 → keep a 4-voxel border from world edges.
        let tx = ((prng >> 8) % (WORLD - 10)) + 5;
        prng = prng.wrapping_mul(0x9E37_79B9).wrapping_add(0x1234_5678);
        let tz = ((prng >> 8) % (WORLD - 10)) + 5;
        let h = terrain_height(tx, tz);
        // Skip trees in low / underwater spots — keeps them out of
        // ditches and on the visibly-grass tiles.
        if h >= 8 {
            plant_tree(tx, tz, h, prng);
            planted += 1;
        }
    }

    // ── Title scene ───────────────────────────────────────────
    //
    // A clean void with the title text floating at world-center. The
    // render() callback orbits a camera around it. FIRE pulls the
    // player into SCENE_GAME (handled in update()).
    //
    // Title text uses FONT_DCP1 (16×18 chiseled-serif). The subtitle
    // uses FONT_ANSI for the smaller "PRESS FIRE" line. Both go in
    // the XY plane, so the +Z face is what the orbit camera reads when
    // it passes through cam_yaw == 0.
    //
    // face_color is painted on the slice closest to the lower coord on
    // the extrusion axis. To put the emissive face on the +Z side
    // (the side the camera sees from cam_yaw≈0), the cart passes the
    // dark body material as face_color and the bright face material
    // as the main color — the spec's documented front/back swap.
    scene_set_active(SCENE_TITLE);
    let title_extents = measure(&FONT_DCP1, 2, 12, "voxlconsl");
    let title_origin = UVec3::new(
        256u32.saturating_sub(title_extents.x as u32 / 2),
        256u32.saturating_sub(title_extents.y as u32 / 2),
        256u32.saturating_sub(title_extents.z as u32 / 2),
    );
    paint_world(
        &FONT_DCP1,
        title_origin,
        Axis::XY,
        M_SIGN_FACE,
        Some(M_SIGN_BODY),
        2,         // 2× scale → 32×36 voxel letters, 9 chars × 32 = 288 wide
        12,        // depth — chunky 3D slab
        "voxlconsl",
    );

    let sub_extents = measure(&FONT_ANSI, 1, 4, "PRESS FIRE");
    let sub_origin = UVec3::new(
        256u32.saturating_sub(sub_extents.x as u32 / 2),
        title_origin.y.saturating_sub(20),  // below the main title
        title_origin.z + 4,                  // sits in front of title's mid-depth
    );
    paint_world(
        &FONT_ANSI,
        sub_origin,
        Axis::XY,
        M_SIGN_FACE,
        None,
        1,
        4,
        "PRESS FIRE",
    );

    // Switch back to the gameplay scene to define the player prefab and
    // spawn the actor; the title scene stays clean of game-world data.
    scene_set_active(SCENE_GAME);

    // ── Player prefab + actor ─────────────────────────────────
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
    }
    let id = actor_spawn_from(P_IDLE, Orientation::Up).expect("player");
    unsafe {
        // Drop the player on the surface at the centre.
        let h = terrain_height(256, 256);
        PLAYER_POS = Vec3::new(254.0, h as f32, 254.0);
        PLAYER = Some(id);
        actor_set_position(id, PLAYER_POS);
        CURRENT_FRAME = P_IDLE;
        // Hide the dude until the player presses FIRE; actors are
        // cart-global and we don't want him in the title scene's frame.
        actor_set_visible(id, false);
    }

    // ── Input ─────────────────────────────────────────────────
    unsafe {
        MOVE_ACTION = input_declare_action(ActionKind::Axis2D, BindingHint::PrimaryMovement, "move");
        AIM_ACTION  = input_declare_action(ActionKind::Axis2D, BindingHint::Aim, "aim");
        FIRE_ACTION = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire, "fire");
    }

    // Boot into the title screen.
    scene_set_active(SCENE_TITLE);
}

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    // Title-screen state: orbit camera + wait for FIRE to start the game.
    if unsafe { STATE } == GameState::Title {
        unsafe { TITLE_CLOCK_MS = TITLE_CLOCK_MS.saturating_add(dt_ms); }
        if input_action_pressed(unsafe { FIRE_ACTION }) {
            unsafe {
                STATE = GameState::Playing;
                if let Some(p) = PLAYER {
                    actor_set_visible(p, true);
                }
            }
            scene_set_active(SCENE_GAME);
        }
        return;
    }

    let dt = (dt_ms as f32) / 1000.0;
    let (mx, my) = input_action_axis2d(unsafe { MOVE_ACTION });
    let (ax, ay) = input_action_axis2d(unsafe { AIM_ACTION });

    unsafe {
        CAM_YAW += ax * 0.005;
        // Non-inverted Y (FPS feel): mouse down → look down. In orbit
        // terms, look-down means the eye sits above the target →
        // pitch positive → pitch *increases* with positive ay.
        CAM_PITCH = (CAM_PITCH + ay * 0.005).clamp(-1.2, 1.2);
        // Hold FIRE to dolly the camera in/out.
        if input_action_button(FIRE_ACTION) {
            CAM_DISTANCE = (CAM_DISTANCE + my * 30.0 * dt).clamp(8.0, 80.0);
        }
    }

    let cam_yaw = unsafe { CAM_YAW };
    // forward = the direction the camera is *looking*, not the direction
    // the camera *sits in* relative to target. Orbit-cam puts the eye at
    // (sin*d, _, cos*d) from target, so the look direction is the
    // negation of that — and that's what W should move you along.
    let forward = Vec3::new(-sine(cam_yaw), 0.0, -cosine(cam_yaw));
    let right   = Vec3::new(cosine(cam_yaw), 0.0, -sine(cam_yaw));
    let movement = Vec3::new(
        right.x * mx + forward.x * my,
        0.0,
        right.z * mx + forward.z * my,
    );
    let speed = 12.0_f32;
    let speed_sq = movement.x * movement.x + movement.z * movement.z;
    let move_active = !unsafe { input_action_button(FIRE_ACTION) };  // FIRE held = camera dolly, not movement

    if let Some(player) = unsafe { PLAYER } {
        unsafe {
            let moving = move_active && speed_sq > 0.0025;

            if move_active {
                PLAYER_POS.x = (PLAYER_POS.x + movement.x * speed * dt).clamp(2.0, (WORLD - 7) as f32);
                PLAYER_POS.z = (PLAYER_POS.z + movement.z * speed * dt).clamp(2.0, (WORLD - 5) as f32);
            }
            // Sample the heightmap each frame so the dude tracks the terrain.
            let h = terrain_height(PLAYER_POS.x as u32, PLAYER_POS.z as u32);
            PLAYER_POS.y = h as f32;
            actor_set_position(player, PLAYER_POS);
            if moving {
                PLAYER_FACING = -atan2(movement.x, movement.z);
                actor_set_yaw(player, PLAYER_FACING);
            }

            // Animate while moving, snap back to idle when stopped.
            // Only call set_prefab on transitions — the swap is cheap
            // but spamming it is wasteful.
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

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    // Title screen: orbit a camera around the floating title text. The
    // sign sits at world-center; we sweep yaw slowly and tilt slightly
    // up to show the chiseled-serif tops.
    if unsafe { STATE } == GameState::Title {
        let t = unsafe { TITLE_CLOCK_MS } as f32 / 1000.0;
        // The title is a flat slab in the XY plane — only its +Z face
        // reads as letters; full-orbit views show edge-on ribs. Sway
        // gently within ±15° instead so the camera always looks at the
        // face, with a touch of motion to feel alive.
        let yaw = sine(t * 0.4) * 0.26;
        // Subtitle sits 20 voxels below the title (y≈218). Target a
        // point between them so the vertical FOV frames both.
        let target = Vec3::new(256.0, 248.0, 256.0);
        let dist = 240.0;
        let cam_pitch = 0.06;
        let cos_pitch = cosine(cam_pitch);
        let eye = Vec3::new(
            target.x + dist * sine(yaw) * cos_pitch,
            target.y + dist * sine(cam_pitch),
            target.z + dist * cosine(yaw) * cos_pitch,
        );
        camera_set_lookat(eye, target, Vec3::Y);
        camera_set_fov(50.0);
        return;
    }

    let (yaw, pitch, dist) = unsafe { (CAM_YAW, CAM_PITCH, CAM_DISTANCE) };
    let pos = unsafe { PLAYER_POS };

    let cos_pitch = cosine(pitch);
    let target = Vec3::new(pos.x + 2.5, pos.y + 4.0, pos.z + 1.5);
    let eye = Vec3::new(
        target.x + dist * sine(yaw) * cos_pitch,
        target.y + dist * sine(pitch),
        target.z + dist * cosine(yaw) * cos_pitch,
    );

    camera_set_lookat(eye, target, Vec3::Y);
    camera_set_fov(60.0);
}

// ── Terrain noise ───────────────────────────────────────────────────────

/// Hash 2D integer coords into a deterministic float in [0, 1).
fn hash2(ix: i32, iz: i32) -> f32 {
    let mut h = (ix as u32)
        .wrapping_mul(0x1657_8E37)
        .wrapping_add((iz as u32).wrapping_mul(0xB7E1_5163));
    h ^= h >> 13;
    h = h.wrapping_mul(0x4BC0_3937);
    h ^= h >> 16;
    (h as f32) * (1.0 / 4_294_967_296.0)
}

fn smoothstep(t: f32) -> f32 { t * t * (3.0 - 2.0 * t) }

fn value_noise_2d(x: f32, z: f32) -> f32 {
    // Manual floor for non-negative inputs (we never sample negative
    // coords; std::f32::floor isn't available in no_std without libm).
    let ix = x as i32;
    let iz = z as i32;
    let fx = x - ix as f32;
    let fz = z - iz as f32;

    let v00 = hash2(ix,     iz);
    let v10 = hash2(ix + 1, iz);
    let v01 = hash2(ix,     iz + 1);
    let v11 = hash2(ix + 1, iz + 1);

    let sx = smoothstep(fx);
    let sz = smoothstep(fz);

    let a = v00 + (v10 - v00) * sx;
    let b = v01 + (v11 - v01) * sx;
    a + (b - a) * sz
}

/// Multi-octave value noise → integer height in [4, 28].
fn terrain_height(x: u32, z: u32) -> u32 {
    let mut h = 0.0_f32;
    let mut amp = 1.0_f32;
    let mut freq = 1.0_f32 / 64.0;
    let mut total = 0.0_f32;
    let mut octave = 0;
    while octave < 4 {
        h += value_noise_2d(x as f32 * freq, z as f32 * freq) * amp;
        total += amp;
        amp *= 0.5;
        freq *= 2.0;
        octave += 1;
    }
    h /= total;
    let v = 4.0 + h * 24.0;
    v as u32
}

// ── Trees + player prefab ───────────────────────────────────────────────

/// Plant a tree at `(cx, cz)` with its base at world y=`base`. `variant`
/// (any u32) drives a small height variation so the forest doesn't look
/// like a stamp pattern. Total tree height ≈ 8–10 voxels (taller than
/// the 7-tall dude); 4-layer canopy shrinking from a 7×7 mid-ring to a
/// 3×3 cap.
fn plant_tree(cx: u32, cz: u32, base: u32, variant: u32) {
    let trunk_h = 4 + (variant % 3);  // 4, 5, or 6
    let trunk_top = base + trunk_h;
    // Trunk: single wood column.
    fill_box(
        UVec3::new(cx, base, cz),
        UVec3::new(cx, trunk_top - 1, cz),
        M_WOOD,
    );
    // 4-layer canopy starting at the trunk top.
    let l0 = trunk_top;
    let l1 = trunk_top + 1;
    let l2 = trunk_top + 2;
    let l3 = trunk_top + 3;
    // 5×5 base
    fill_box(UVec3::new(cx - 2, l0, cz - 2), UVec3::new(cx + 2, l0, cz + 2), M_LEAF);
    // 7×7 mid ring — the visually dominant layer
    fill_box(UVec3::new(cx - 3, l1, cz - 3), UVec3::new(cx + 3, l1, cz + 3), M_LEAF);
    // 5×5 upper
    fill_box(UVec3::new(cx - 2, l2, cz - 2), UVec3::new(cx + 2, l2, cz + 2), M_LEAF);
    // 3×3 cap
    fill_box(UVec3::new(cx - 1, l3, cz - 1), UVec3::new(cx + 1, l3, cz + 1), M_LEAF);
}

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
    let mut x = 1; while x <= 3 { let mut y = 2; while y <= 4 { put(buf, x, y, 1, M_SHIRT); y += 1; } x += 1; }
    // Arms (x=0/4, y=2..3) at the swing offset
    put(buf, 0, 2, arm_l_z, M_SHIRT); put(buf, 0, 3, arm_l_z, M_SHIRT);
    put(buf, 4, 2, arm_r_z, M_SHIRT); put(buf, 4, 3, arm_r_z, M_SHIRT);
    // Head 3×2×3 (x=1..3, y=5..6, full z)
    let mut x = 1; while x <= 3 {
        let mut y = 5; while y <= 6 {
            let mut z = 0; while z < DUDE_D { put(buf, x, y, z, M_SKIN); z += 1; }
            y += 1;
        }
        x += 1;
    }
}

// ── tiny no_std math ─────────────────────────────────────────────────────

fn sine(x: f32) -> f32 {
    let two_pi = core::f32::consts::TAU;
    let mut x = x % two_pi;
    if x > core::f32::consts::PI { x -= two_pi; }
    if x < -core::f32::consts::PI { x += two_pi; }
    let x2 = x * x;
    x * (1.0 - x2 * (1.0 / 6.0 - x2 * (1.0 / 120.0 - x2 / 5040.0)))
}
fn cosine(x: f32) -> f32 { sine(x + core::f32::consts::FRAC_PI_2) }

fn atan2(y: f32, x: f32) -> f32 {
    if x == 0.0 && y == 0.0 { return 0.0; }
    let abs_x = if x < 0.0 { -x } else { x };
    let abs_y = if y < 0.0 { -y } else { y };
    let (a, swapped) = if abs_x > abs_y { (abs_y / abs_x, false) } else { (abs_x / abs_y, true) };
    let r = a * (0.97 - 0.19 * a * a);
    let r = if swapped { core::f32::consts::FRAC_PI_2 - r } else { r };
    let r = if x < 0.0 { core::f32::consts::PI - r } else { r };
    if y < 0.0 { -r } else { r }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log("big-world cart panicked");
    let _ = info;
    loop {}
}
