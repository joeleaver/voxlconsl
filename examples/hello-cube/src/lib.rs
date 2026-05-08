//! hello-cube — a first voxlconsl cart with a controllable, animated
//! character.
//!
//! Builds a chequered ground, a voxel tree with leaf canopy, a ruby on top,
//! and a few gold cubes scattered around. The player is spawned from a
//! **prefab** (§11.4) and animates via **flipbook prefab-swap** (§11.9):
//! while moving, the cart cycles the actor through walk-cycle prefabs;
//! when standing still it swaps back to the idle pose. Multiple instances
//! of the same prefab share a single baked volume via copy-on-write — the
//! prefab swap is a pointer rotation, not a re-bake.

#![no_std]
#![no_main]

use voxlconsl_sdk::*;
use voxlconsl_sdk::animation::Flipbook;

/// Side length of the playable area in voxels. Each scene is 512³
/// total per the spec; the demo only uses a 64×64×N square at the
/// (0,0,0) corner so the player can walk across the chunk-boundary
/// at x=32 / z=32 and you can see multi-chunk traversal at work.
const PLAY_SIDE: u32 = 64;

/// Scene IDs the cart uses. Scene 0 = the lit overworld, Scene 1 = a
/// small stone "dungeon" room. FIRE toggles between them.
const SCENE_OVERWORLD: SceneId = SceneId(0);
const SCENE_DUNGEON:   SceneId = SceneId(1);

const M_STONE: u8 = 1;
const M_WOOD:  u8 = 2;
const M_LEAF:  u8 = 3;
const M_RUBY:  u8 = 4;
const M_GOLD:  u8 = 5;
const M_GRASS: u8 = 6;
const M_SKIN:  u8 = 7;
const M_SHIRT: u8 = 8;

// ── Player prefab geometry ────────────────────────────────────────────────
//
// The dude is 5×7×3: x=5 wide, y=7 tall, z=3 deep. Animation cycles the
// foot offsets in z to fake a walk.
const DUDE_W: usize = 5;
const DUDE_H: usize = 7;
const DUDE_D: usize = 3;
const DUDE_VOL: usize = DUDE_W * DUDE_H * DUDE_D;

const P_IDLE:    PrefabId = PrefabId(1);
const P_WALK_0:  PrefabId = PrefabId(2);
const P_WALK_1:  PrefabId = PrefabId(3);
const P_WALK_2:  PrefabId = PrefabId(4);
const P_BARREL:  PrefabId = PrefabId(5);

// Barrel prefab geometry — 4×6×4 with stained ends so the orientation
// reads visually. Same prefab spawned at three different orientations
// shows the 24-orientation bake at work.
const BARREL_W: usize = 4;
const BARREL_H: usize = 6;
const BARREL_D: usize = 4;
const BARREL_VOL: usize = BARREL_W * BARREL_H * BARREL_D;
static mut DENSE_BARREL: [u8; BARREL_VOL] = [0; BARREL_VOL];

// Authored at runtime in `init` and registered with the host via
// `prefab_define`. Cart is no_std + no_alloc, so the buffers live in
// `static mut`.
static mut DENSE_IDLE:   [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_WALK_0: [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_WALK_1: [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_WALK_2: [u8; DUDE_VOL] = [0; DUDE_VOL];

// Camera state — orbit around the dude.
static mut CAM_YAW: f32 = 0.7;
static mut CAM_PITCH: f32 = 0.5;
static mut CAM_DISTANCE: f32 = 14.0;

// The player actor.
static mut PLAYER: Option<ActorId> = None;
static mut PLAYER_POS: Vec3 = Vec3 { x: 16.0, y: 1.0, z: 16.0 };
static mut PLAYER_FACING: f32 = 0.0;

// Walk-cycle flipbook: WALK_0 → WALK_1 → WALK_2 → WALK_1 → repeat.
const WALK_FRAMES: &[PrefabId] = &[P_WALK_0, P_WALK_1, P_WALK_2, P_WALK_1];
static mut WALK_FB: Flipbook = Flipbook::new(WALK_FRAMES, 140, true);

// Tracks which prefab is currently bound so we don't spam `actor_set_prefab`.
static mut CURRENT_FRAME: PrefabId = P_IDLE;

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
    //
    // 64×64 chequered ground spans four chunks (cx 0..1 × cz 0..1) so
    // the player walking around proves the multi-chunk renderer works.
    for x in 0..PLAY_SIDE {
        for z in 0..PLAY_SIDE {
            let m = if (x + z) % 2 == 0 { M_STONE } else { M_GRASS };
            set_voxel(UVec3::new(x, 0, z), m);
        }
    }

    // Two trees, one in chunk (0,0,0) and a second across the boundary
    // in chunk (1,0,1) so a single ray heading northeast traces both.
    plant_tree(8, 24);
    plant_tree(48, 48);

    for &(x, z) in &[(4u32, 4), (28, 6), (5, 27), (26, 26), (12, 22), (44, 38), (52, 16)] {
        set_voxel(UVec3::new(x, 1, z), M_GOLD);
    }

    // ── Scene 1: a small "dungeon" room ──────────────────────
    //
    // Demonstrates the multi-scene model (§3.7): the cart authors
    // both scenes up front and toggles between them at runtime via
    // FIRE. Materials, prefabs, and actors are cart-global, so the
    // dude survives the switch — but the voxel grid swaps wholesale.
    scene_set_active(SCENE_DUNGEON);
    fill_box(UVec3::new(8, 0, 8), UVec3::new(40, 0, 40), M_STONE);
    // Walls: 4-tall stone perimeter at the room's edges.
    fill_box(UVec3::new(8, 1, 8),  UVec3::new(40, 4, 8),  M_STONE);
    fill_box(UVec3::new(8, 1, 40), UVec3::new(40, 4, 40), M_STONE);
    fill_box(UVec3::new(8, 1, 8),  UVec3::new(8, 4, 40),  M_STONE);
    fill_box(UVec3::new(40, 1, 8), UVec3::new(40, 4, 40), M_STONE);
    // Glowing ruby pillar at the room's center.
    for y in 1..5 {
        set_voxel(UVec3::new(24, y, 24), M_RUBY);
    }
    // Switch back to the overworld so first-frame render shows scene 0.
    scene_set_active(SCENE_OVERWORLD);

    // ── Barrel prefab + three barrels at different orientations ──
    //
    // The barrel's top is RUBY and its bottom is WOOD. With three
    // copies spawned at Up, EastUp, and NorthUp, the ruby cap should
    // visibly point in three different directions, demonstrating the
    // 24-orientation bake (§11.3 / §11.5). All three instances share
    // the same prefab data; the host bakes one volume per unique
    // orientation and Rc-shares any duplicates.
    unsafe {
        build_barrel(&mut *(&raw mut DENSE_BARREL));
        prefab_define(
            P_BARREL,
            &*(&raw const DENSE_BARREL),
            U8Vec3::new(BARREL_W as u8, BARREL_H as u8, BARREL_D as u8),
        );
    }
    if let Some(b) = actor_spawn_from(P_BARREL, Orientation::Up) {
        actor_set_position(b, Vec3::new(2.0, 1.0, 4.0));
    }
    if let Some(b) = actor_spawn_from(P_BARREL, Orientation::EastUp) {
        actor_set_position(b, Vec3::new(2.0, 1.0, 12.0));
    }
    if let Some(b) = actor_spawn_from(P_BARREL, Orientation::NorthUp) {
        actor_set_position(b, Vec3::new(2.0, 1.0, 20.0));
    }
    // Far-chunk barrel proves the macro-grid is binning into cell
    // (1, 0, 1) and the renderer's chunk traversal reaches it.
    if let Some(b) = actor_spawn_from(P_BARREL, Orientation::Up) {
        actor_set_position(b, Vec3::new(58.0, 1.0, 56.0));
    }

    // ── Player prefabs ────────────────────────────────────────
    // IDLE: legs straight (z=1, z=1), arms at sides (z=1, z=1).
    // WALK frames: feet/arms swing in counterphase so the cycle reads.
    unsafe {
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

    // ── Player actor (spawned from prefab; CoW = the host shares one
    // baked volume between this actor and any future instances) ──
    let id = actor_spawn_from(P_IDLE, Orientation::Up).expect("failed to spawn player");
    unsafe {
        PLAYER = Some(id);
        actor_set_position(id, PLAYER_POS);
        CURRENT_FRAME = P_IDLE;
    }

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
        // Non-inverted Y (FPS feel): mouse down → look down. In orbit
        // terms, look-down means the eye sits above the target →
        // pitch positive → pitch *increases* with positive ay.
        CAM_PITCH += ay * 0.005;
        CAM_PITCH = CAM_PITCH.clamp(-1.2, 1.2);
    }

    // WASD drives the dude relative to camera-facing direction.
    let move_speed = 6.0_f32;
    let cam_yaw = unsafe { CAM_YAW };
    // forward = where the camera *looks*, not where the eye sits. Orbit
    // cam puts the eye at (sin*d, _, cos*d) from target, so look_dir
    // is the negation; W must move along look_dir for "forward" to
    // mean what the player sees.
    let forward = Vec3::new(-sine(cam_yaw), 0.0, -cosine(cam_yaw));
    let right = Vec3::new(cosine(cam_yaw), 0.0, -sine(cam_yaw));

    let movement = Vec3::new(
        right.x * mx + forward.x * my,
        0.0,
        right.z * mx + forward.z * my,
    );
    let speed_sq = movement.x * movement.x + movement.z * movement.z;
    let moving = speed_sq > 0.0025;

    if let Some(player) = unsafe { PLAYER } {
        unsafe {
            PLAYER_POS.x = (PLAYER_POS.x + movement.x * move_speed * dt).clamp(0.0, PLAY_SIDE as f32 - 5.0);
            PLAYER_POS.z = (PLAYER_POS.z + movement.z * move_speed * dt).clamp(0.0, PLAY_SIDE as f32 - 3.0);
            actor_set_position(player, PLAYER_POS);

            // Face the direction of movement when walking.
            if moving {
                PLAYER_FACING = -atan2(movement.x, movement.z);
                actor_set_yaw(player, PLAYER_FACING);
            }

            // Animation: cycle walk frames while moving, snap back to
            // idle when stopped. Only call `actor_set_prefab` when the
            // bound frame actually changes — the swap is cheap (a
            // pointer move on the host) but spamming it is silly.
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
        // dungeon scene. The player actor is cart-global, so it
        // survives the switch — but the world geometry changes
        // wholesale, demonstrating §3.7's "host swaps voxel grids"
        // model. Reposition the player to a known-safe spot in
        // each scene since the dungeon room is centered at (24, 1, 24).
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

    camera_set_lookat(eye, target, Vec3::Y);
    camera_set_fov(60.0);
}

// ── Prefab authoring ───────────────────────────────────────────────────────

/// Index into a 5×7×3 dense buffer (row-major: x fastest, then y, then z).
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
/// `left_leg_z` / `right_leg_z` are 0..=2 (front/middle/back), same for
/// arms. Idle frame uses z=1 for everything; walk frames swing legs and
/// arms in counterphase.
fn build_dude(
    buf: &mut [u8; DUDE_VOL],
    left_leg_z: usize, right_leg_z: usize,
    arm_l_z: usize, arm_r_z: usize,
) {
    *buf = [0; DUDE_VOL];

    // Legs (y 0..=1)
    put(buf, 1, 0, left_leg_z,  M_WOOD);
    put(buf, 1, 1, left_leg_z,  M_WOOD);
    put(buf, 3, 0, right_leg_z, M_WOOD);
    put(buf, 3, 1, right_leg_z, M_WOOD);

    // Torso (x 1..=3, y 2..=4, z = 1)
    let mut x = 1;
    while x <= 3 {
        let mut y = 2;
        while y <= 4 {
            put(buf, x, y, 1, M_SHIRT);
            y += 1;
        }
        x += 1;
    }

    // Arms (x = 0 / 4, y 2..=3)
    put(buf, 0, 2, arm_l_z, M_SHIRT);
    put(buf, 0, 3, arm_l_z, M_SHIRT);
    put(buf, 4, 2, arm_r_z, M_SHIRT);
    put(buf, 4, 3, arm_r_z, M_SHIRT);

    // Head (x 1..=3, y 5..=6, full z)
    let mut x = 1;
    while x <= 3 {
        let mut y = 5;
        while y <= 6 {
            let mut z = 0;
            while z < DUDE_D {
                put(buf, x, y, z, M_SKIN);
                z += 1;
            }
            y += 1;
        }
        x += 1;
    }
}

/// Plant a 5-tall trunk + leaf canopy + ruby cap centered at `(cx, cz)`.
fn plant_tree(cx: u32, cz: u32) {
    for y in 1..6 { set_voxel(UVec3::new(cx, y, cz), M_WOOD); }
    let mut dy = 0;
    while dy < 3 {
        let r: i32 = if dy == 1 { 2 } else { 1 };
        let y = 6 + dy as u32;
        let mut dx = -r;
        while dx <= r {
            let mut dz = -r;
            while dz <= r {
                if dx * dx + dz * dz <= r * r + 1 {
                    let x = (cx as i32 + dx) as u32;
                    let z = (cz as i32 + dz) as u32;
                    set_voxel(UVec3::new(x, y, z), M_LEAF);
                }
                dz += 1;
            }
            dx += 1;
        }
        dy += 1;
    }
    set_voxel(UVec3::new(cx, 9, cz), M_RUBY);
}

/// Build the barrel prefab into `buf`: WOOD bottom (y=0), GOLD body
/// (y=1..=4), RUBY top (y=5). The contrasting top/bottom and the gold
/// midriff make rotations easy to read.
fn build_barrel(buf: &mut [u8; BARREL_VOL]) {
    *buf = [0; BARREL_VOL];
    let bx = BARREL_W;
    let by = BARREL_H;
    for z in 0..BARREL_D {
        for x in 0..bx {
            let mut y = 0;
            while y < by {
                let m = match y {
                    0 => M_WOOD,            // bottom
                    1 | 2 | 3 | 4 => M_GOLD, // body
                    _ => M_RUBY,            // top (y=5)
                };
                buf[(z * by + y) * bx + x] = m;
                y += 1;
            }
        }
    }
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
