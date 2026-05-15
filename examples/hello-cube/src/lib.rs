//! hello-cube — the voxlconsl SDK reference cart.
//!
//! This is the smallest cart that touches a useful slice of the SDK,
//! and the recommended starting point for anyone learning the engine.
//! It builds a chequered ground, two trees with leaf canopies + ruby
//! caps, a handful of gold cubes, three orientation-rotated barrels,
//! and a controllable little dude. Pressing FIRE toggles between the
//! lit overworld and a small stone dungeon scene.
//!
//! ## What this cart demonstrates
//!
//! - **Materials defined in code** — no materials.toml, so a reader
//!   can see the full setup in one file. The bigger carts (big-world
//!   / voxdude) use the asset-pipeline approach instead.
//! - **Voxel world building** — `set_voxel`, `fill_box`, simple
//!   procedural tree planting.
//! - **Scenes** (§3.7) — two voxel grids on one cart; FIRE flips
//!   between them. Materials, prefabs, and the player actor are
//!   cart-global and survive the switch.
//! - **Prefabs + CoW + orientations** (§11.3-§11.5) — one barrel
//!   prefab spawned three times at Up/EastUp/NorthUp shows the
//!   24-orientation bake at work.
//! - **Flipbook animation** (§11.9) — the dude cycles four prefab
//!   frames while moving and snaps back to idle when stopped.
//! - **Camera-relative movement** — WASD reads as forward/strafe
//!   relative to the orbit camera's yaw, not world axes.
//! - **Multi-chunk world** — the 64×64 ground straddles a chunk
//!   boundary; the far barrel proves macro-grid binning + the
//!   renderer's chunk traversal reach the (1, 0, 1) chunk.

#![no_std]
#![no_main]

use voxlconsl_sdk::*;
use voxlconsl_sdk::animation::Flipbook;

// ── World / scene constants ──────────────────────────────────────

/// Side length of the playable ground. Each scene is 512³ total per
/// the spec; we use a 64×64 square at the (0,0,0) corner so the player
/// can walk across the chunk boundary at x=32 / z=32 and see
/// multi-chunk traversal at work.
const PLAY_SIDE: u32 = 64;

const SCENE_OVERWORLD: SceneId = SceneId(0);
const SCENE_DUNGEON:   SceneId = SceneId(1);

// ── Material slot table ──────────────────────────────────────────

const M_STONE: u8 = 1;
const M_WOOD:  u8 = 2;
const M_LEAF:  u8 = 3;
const M_RUBY:  u8 = 4;
const M_GOLD:  u8 = 5;
const M_GRASS: u8 = 6;
const M_SKIN:  u8 = 7;
const M_SHIRT: u8 = 8;

// ── Prefab geometry ──────────────────────────────────────────────
//
// Dude is 5×7×3 (wide × tall × deep). The walk animation swings the
// foot offsets in z to fake a stride.
const DUDE_W: usize = 5;
const DUDE_H: usize = 7;
const DUDE_D: usize = 3;
const DUDE_VOL: usize = DUDE_W * DUDE_H * DUDE_D;

// Barrel is 4×6×4 with stained top and bottom so orientation reads
// visually — the same prefab spawned at three different orientations
// shows the 24-orientation bake at work.
const BARREL_W: usize = 4;
const BARREL_H: usize = 6;
const BARREL_D: usize = 4;
const BARREL_VOL: usize = BARREL_W * BARREL_H * BARREL_D;

const P_IDLE:   PrefabId = PrefabId(1);
const P_WALK_0: PrefabId = PrefabId(2);
const P_WALK_1: PrefabId = PrefabId(3);
const P_WALK_2: PrefabId = PrefabId(4);
const P_BARREL: PrefabId = PrefabId(5);

// Dense buffers authored at runtime in `init` and handed to
// `prefab_define`. The cart is no_std + no_alloc, so the buffers
// live in `static mut`.
static mut DENSE_BARREL: [u8; BARREL_VOL] = [0; BARREL_VOL];
static mut DENSE_IDLE:   [u8; DUDE_VOL]   = [0; DUDE_VOL];
static mut DENSE_WALK_0: [u8; DUDE_VOL]   = [0; DUDE_VOL];
static mut DENSE_WALK_1: [u8; DUDE_VOL]   = [0; DUDE_VOL];
static mut DENSE_WALK_2: [u8; DUDE_VOL]   = [0; DUDE_VOL];

// ── Runtime state ────────────────────────────────────────────────

// Camera orbits the dude.
static mut CAM_YAW:      f32 = 0.7;
static mut CAM_PITCH:    f32 = 0.5;
static mut CAM_DISTANCE: f32 = 14.0;

static mut PLAYER:        Option<ActorId> = None;
static mut PLAYER_POS:    Vec3 = Vec3 { x: 16.0, y: 1.0, z: 16.0 };
static mut PLAYER_FACING: f32  = 0.0;

const WALK_FRAMES: &[PrefabId] = &[P_WALK_0, P_WALK_1, P_WALK_2, P_WALK_1];
static mut WALK_FB: Flipbook = Flipbook::new(WALK_FRAMES, 140, true);
/// The prefab currently bound to the player actor — checked each
/// frame so we only call `actor_set_prefab` on transitions.
static mut CURRENT_FRAME: PrefabId = P_IDLE;

static mut MOVE_ACTION: ActionHandle = ActionHandle(0);
static mut AIM_ACTION:  ActionHandle = ActionHandle(0);
static mut FIRE_ACTION: ActionHandle = ActionHandle(0);

// ── Boot ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    setup_materials();
    sky_set_gradient(Material::pack_color(7, 0), Material::pack_color(6, 0));
    light_set_sun(Vec3::new(-0.6, 0.8, 0.4), 0, 0);

    paint_overworld();
    paint_dungeon();
    // Switch back to the overworld so the first-frame render shows scene 0.
    scene_set_active(SCENE_OVERWORLD);

    spawn_barrels();
    spawn_player();
    register_inputs();
}

fn setup_materials() {
    material_define(M_STONE, Material::pack_color(14, 1), 0, MaterialFlags::empty());
    material_define(M_WOOD,  Material::pack_color( 0, 1), 0, MaterialFlags::empty());
    material_define(M_LEAF,  Material::pack_color( 2, 2), 0, MaterialFlags::empty());
    material_define(M_RUBY,  Material::pack_color(10, 2), 6, MaterialFlags::empty().with(MaterialFlags::GLOSSY));
    material_define(M_GOLD,  Material::pack_color(12, 3), 0, MaterialFlags::empty().with(MaterialFlags::GLOSSY));
    material_define(M_GRASS, Material::pack_color( 3, 2), 0, MaterialFlags::empty());
    material_define(M_SKIN,  Material::pack_color( 1, 3), 0, MaterialFlags::empty());
    material_define(M_SHIRT, Material::pack_color( 7, 2), 0, MaterialFlags::empty());
}

/// Paint scene 0: a chequered 64×64 ground, two trees straddling the
/// chunk boundary, and a handful of gold cubes scattered on top.
fn paint_overworld() {
    for x in 0..PLAY_SIDE {
        for z in 0..PLAY_SIDE {
            let m = if (x + z) % 2 == 0 { M_STONE } else { M_GRASS };
            set_voxel(UVec3::new(x, 0, z), m);
        }
    }

    // Two trees — one in chunk (0,0,0), one across the boundary in
    // chunk (1,0,1) — so a single northeast-bound ray traces both.
    plant_tree(8, 24);
    plant_tree(48, 48);

    for &(x, z) in &[(4u32, 4), (28, 6), (5, 27), (26, 26), (12, 22), (44, 38), (52, 16)] {
        set_voxel(UVec3::new(x, 1, z), M_GOLD);
    }
}

/// Paint scene 1: a small stone room with a glowing ruby pillar at
/// its centre. Demonstrates §3.7's "host swaps voxel grids" model —
/// materials, prefabs, and actors live cart-global so the dude
/// survives the scene flip, but the voxel grid swaps wholesale.
fn paint_dungeon() {
    scene_set_active(SCENE_DUNGEON);
    fill_box(UVec3::new(8, 0, 8), UVec3::new(40, 0, 40), M_STONE);
    // 4-tall stone perimeter walls.
    fill_box(UVec3::new(8,  1, 8),  UVec3::new(40, 4, 8),  M_STONE);
    fill_box(UVec3::new(8,  1, 40), UVec3::new(40, 4, 40), M_STONE);
    fill_box(UVec3::new(8,  1, 8),  UVec3::new(8,  4, 40), M_STONE);
    fill_box(UVec3::new(40, 1, 8),  UVec3::new(40, 4, 40), M_STONE);
    for y in 1..5 {
        set_voxel(UVec3::new(24, y, 24), M_RUBY);
    }
}

/// Bake the barrel prefab and spawn four instances. Three of them sit
/// at Up / EastUp / NorthUp orientations so the ruby cap visibly
/// points in three different directions — the 24-orientation bake at
/// work. The fourth sits in the far chunk to prove the macro-grid
/// binning + chunk-traversal reach chunk (1, 0, 1).
fn spawn_barrels() {
    unsafe {
        build_barrel(&mut *(&raw mut DENSE_BARREL));
        prefab_define(
            P_BARREL,
            &*(&raw const DENSE_BARREL),
            U8Vec3::new(BARREL_W as u8, BARREL_H as u8, BARREL_D as u8),
        );
    }
    let placements = [
        (Orientation::Up,      Vec3::new( 2.0, 1.0,  4.0)),
        (Orientation::EastUp,  Vec3::new( 2.0, 1.0, 12.0)),
        (Orientation::NorthUp, Vec3::new( 2.0, 1.0, 20.0)),
        (Orientation::Up,      Vec3::new(58.0, 1.0, 56.0)),
    ];
    for (orient, pos) in placements {
        if let Some(b) = actor_spawn_from(P_BARREL, orient) {
            actor_set_position(b, pos);
        }
    }
}

/// Bake the four dude prefab frames and spawn the player actor.
/// All four prefabs share the same dense buffer layout; CoW means
/// the host stores one baked volume per unique frame and shares it
/// across all instances.
fn spawn_player() {
    unsafe {
        // IDLE: legs straight (z=1), arms at sides (z=1).
        // WALK frames: feet/arms swing in counterphase so the cycle reads.
        build_dude(&mut *(&raw mut DENSE_IDLE),   /*l*/ 1, /*r*/ 1, /*al*/ 1, /*ar*/ 1);
        build_dude(&mut *(&raw mut DENSE_WALK_0), /*l*/ 0, /*r*/ 2, /*al*/ 2, /*ar*/ 0);
        build_dude(&mut *(&raw mut DENSE_WALK_1), /*l*/ 1, /*r*/ 1, /*al*/ 1, /*ar*/ 1);
        build_dude(&mut *(&raw mut DENSE_WALK_2), /*l*/ 2, /*r*/ 0, /*al*/ 0, /*ar*/ 2);

        let size = U8Vec3::new(DUDE_W as u8, DUDE_H as u8, DUDE_D as u8);
        prefab_define(P_IDLE,   &*(&raw const DENSE_IDLE),   size);
        prefab_define(P_WALK_0, &*(&raw const DENSE_WALK_0), size);
        prefab_define(P_WALK_1, &*(&raw const DENSE_WALK_1), size);
        prefab_define(P_WALK_2, &*(&raw const DENSE_WALK_2), size);
    }
    let id = actor_spawn_from(P_IDLE, Orientation::Up).expect("failed to spawn player");
    unsafe {
        PLAYER = Some(id);
        actor_set_position(id, PLAYER_POS);
        CURRENT_FRAME = P_IDLE;
    }
}

fn register_inputs() {
    unsafe {
        MOVE_ACTION = input_declare_action(ActionKind::Axis2D, BindingHint::PrimaryMovement, "move");
        AIM_ACTION  = input_declare_action(ActionKind::Axis2D, BindingHint::Aim, "aim");
        FIRE_ACTION = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire, "fire");
    }
}

// ── Per-frame update ─────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    let dt = (dt_ms as f32) / 1000.0;
    let (mx, my) = input_action_axis2d(unsafe { MOVE_ACTION });
    let (ax, ay) = input_action_axis2d(unsafe { AIM_ACTION });

    // Mouse aim → camera yaw/pitch. Non-inverted Y (FPS feel): mouse
    // down → look down. In orbit terms, look-down means the eye sits
    // above the target → pitch positive → pitch increases with `ay`.
    unsafe {
        CAM_YAW += ax * 0.005;
        CAM_PITCH = (CAM_PITCH + ay * 0.005).clamp(-1.2, 1.2);
    }

    // WASD drives the dude relative to camera-facing direction.
    //
    // forward = where the camera *looks*, not where the eye sits. The
    // orbit cam puts the eye at (sin*d, _, cos*d) from target, so
    // look_dir is the negation; W must move along look_dir for
    // "forward" to mean what the player sees.
    let cam_yaw = unsafe { CAM_YAW };
    let forward = Vec3::new(-sine(cam_yaw), 0.0, -cosine(cam_yaw));
    let right   = Vec3::new( cosine(cam_yaw), 0.0, -sine(cam_yaw));
    let movement = Vec3::new(
        right.x * mx + forward.x * my,
        0.0,
        right.z * mx + forward.z * my,
    );
    let move_speed = 6.0_f32;
    let moving = movement.x * movement.x + movement.z * movement.z > 0.0025;

    if let Some(player) = unsafe { PLAYER } {
        unsafe {
            PLAYER_POS.x = (PLAYER_POS.x + movement.x * move_speed * dt).clamp(0.0, PLAY_SIDE as f32 - 5.0);
            PLAYER_POS.z = (PLAYER_POS.z + movement.z * move_speed * dt).clamp(0.0, PLAY_SIDE as f32 - 3.0);
            actor_set_position(player, PLAYER_POS);

            if moving {
                PLAYER_FACING = -atan2(movement.x, movement.z);
                actor_set_yaw(player, PLAYER_FACING);
            }

            // Cycle the walk frames while moving; snap to idle when
            // stopped. Only call `actor_set_prefab` on transitions —
            // the swap is cheap but spamming it is wasteful.
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

        // Edge-detected FIRE toggles between the overworld and the
        // dungeon scene. Player actor is cart-global so it survives
        // the switch — but the voxel grid swaps wholesale. We jump
        // to a known-safe spot in whichever scene we're now in
        // (the dungeon room is centered at (24, 1, 24)).
        if input_action_pressed(unsafe { FIRE_ACTION }) {
            let now = scene_get_active();
            let next = if now == SCENE_OVERWORLD { SCENE_DUNGEON } else { SCENE_OVERWORLD };
            scene_set_active(next);
            unsafe {
                PLAYER_POS = if next == SCENE_DUNGEON {
                    Vec3::new(20.0, 1.0, 20.0)
                } else {
                    Vec3::new(16.0, 1.0, 16.0)
                };
                actor_set_position(player, PLAYER_POS);
            }
        }
    }
}

// ── Render ───────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    let (yaw, pitch, dist) = unsafe { (CAM_YAW, CAM_PITCH, CAM_DISTANCE) };
    let pos = unsafe { PLAYER_POS };

    // Orbit around the player, eye at distance/pitch from chest height.
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

// ── Prefab authoring ─────────────────────────────────────────────

/// Index into a 5×7×3 dense buffer (x fastest, then y, then z —
/// matches `prefab_define`'s expected layout).
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
    put(buf, 1, 0, left_leg_z,  M_WOOD);
    put(buf, 1, 1, left_leg_z,  M_WOOD);
    put(buf, 3, 0, right_leg_z, M_WOOD);
    put(buf, 3, 1, right_leg_z, M_WOOD);

    // Torso (3×3 block, x=1..=3, y=2..=4, z=1)
    for x in 1..=3 {
        for y in 2..=4 {
            put(buf, x, y, 1, M_SHIRT);
        }
    }

    // Arms (x=0/4, y=2..=3) at the swing offset
    put(buf, 0, 2, arm_l_z, M_SHIRT);
    put(buf, 0, 3, arm_l_z, M_SHIRT);
    put(buf, 4, 2, arm_r_z, M_SHIRT);
    put(buf, 4, 3, arm_r_z, M_SHIRT);

    // Head (3×2×3, x=1..=3, y=5..=6, full z)
    for x in 1..=3 {
        for y in 5..=6 {
            for z in 0..DUDE_D {
                put(buf, x, y, z, M_SKIN);
            }
        }
    }
}

/// Plant a 5-tall trunk + 3-layer leaf canopy + ruby cap centred at `(cx, cz)`.
fn plant_tree(cx: u32, cz: u32) {
    for y in 1..6 {
        set_voxel(UVec3::new(cx, y, cz), M_WOOD);
    }
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
}

/// Build the barrel prefab: WOOD bottom, GOLD body, RUBY top.
/// Contrasting end caps make the orientations easy to read.
fn build_barrel(buf: &mut [u8; BARREL_VOL]) {
    *buf = [0; BARREL_VOL];
    for z in 0..BARREL_D {
        for x in 0..BARREL_W {
            for y in 0..BARREL_H {
                let m = match y {
                    0           => M_WOOD,   // bottom
                    1..=4       => M_GOLD,   // body
                    _           => M_RUBY,   // top (y=5)
                };
                buf[(z * BARREL_H + y) * BARREL_W + x] = m;
            }
        }
    }
}

// ── Tiny no_std math ─────────────────────────────────────────────
//
// Good to ~0.001 on `[-π, π]`. The cart is no_std + no_alloc so libm
// isn't available without pulling it in as a dependency. These
// polynomial approximations are plenty for character facing + orbit
// camera math.

fn sine(x: f32) -> f32 {
    let two_pi = core::f32::consts::TAU;
    let mut x = x % two_pi;
    if x >  core::f32::consts::PI { x -= two_pi; }
    if x < -core::f32::consts::PI { x += two_pi; }
    let x2 = x * x;
    x * (1.0 - x2 * (1.0 / 6.0 - x2 * (1.0 / 120.0 - x2 / 5040.0)))
}

fn cosine(x: f32) -> f32 { sine(x + core::f32::consts::FRAC_PI_2) }

/// `atan2` accurate to ~0.01 rad. Sufficient for character facing.
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
