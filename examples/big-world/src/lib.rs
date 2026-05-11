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
use voxlconsl_sdk::physics;
use voxlconsl_sdk::text::{measure, paint_world, Axis, FONT_ANSI, FONT_DCP1};

// Frames-per-sand-drop. With ~22 fps gameplay this gives ~5 drops/s.
// Water uses the same cadence as sand now that the §10.3 liquid rule
// tracks per-voxel fluid level (v0.1.5): the puddle equilibrates flat
// instead of building a pyramid, so the source no longer has to be
// rate-limited.
const SAND_DROP_PERIOD:  u32 = 4;
const WATER_DROP_PERIOD: u32 = 4;
static mut SAND_DROP_COUNTER: u32 = 0;
static mut WATER_DROP_COUNTER: u32 = 0;

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
const M_RETICLE:   u8 = 10;
const M_SAND:      u8 = 11;
const M_WATER:     u8 = 12;
const M_FIRE:      u8 = 13;
const M_EMBER:     u8 = 14;

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

// Camera state — orbit around the dude. Defaults frame the dude
// from a 3rd-person SW vantage so the gameplay HUD reads
// immediately on entry.
static mut CAM_YAW: f32 = 0.7;
static mut CAM_PITCH: f32 = 0.45;
static mut CAM_DISTANCE: f32 = 28.0;

const CAM_PITCH_MIN: f32 = -0.20;     // ~-11° (just above looking up)
const CAM_PITCH_MAX: f32 = 1.20;      // ~+69° (close to top-down)
const CAM_DISTANCE_MIN: f32 = 6.0;
const CAM_DISTANCE_MAX: f32 = 64.0;
// Fraction of current distance applied per wheel-notch.
const ZOOM_PER_NOTCH: f32 = 0.12;

// Targeting reticle — last-painted voxel position, so we can clear it
// before painting the next frame's hit point. None on first frame.
static mut RETICLE_POS: Option<UVec3> = None;

// Action handles.
static mut MOVE_ACTION: ActionHandle = ActionHandle(0);
static mut AIM_ACTION:  ActionHandle = ActionHandle(0);
static mut ZOOM_ACTION: ActionHandle = ActionHandle(0);
static mut FIRE_ACTION: ActionHandle = ActionHandle(0);

// ── Embers ────────────────────────────────────────────────────────────────
// The §10.3 CA only spreads fire cell-by-cell to cardinal neighbors.
// big-world's forest is too sparse for that to ever reach the next
// tree, so the cart adds two layers on top:
//
//   1. Burn sites: cart-known positions where fire is currently
//      active. Per tick, each site rolls to *launch* an ember.
//   2. Embers: airborne `M_EMBER` voxels with a velocity vector.
//      Each tick we clear the ember's previous cell, advance its
//      position, write the new cell, and probe what we hit. If the
//      cell underneath the ember (or the destination cell itself)
//      is `M_LEAF` or `M_WOOD`, we ignite it and the ember dies;
//      anything else solid snuffs the ember. The result is little
//      glowing dots arcing through the canopy from burning tree to
//      neighbouring trees — "embers", essentially.
//
// Embers carry no CA flags so they never enter the §10.3 active
// set; the cart owns them top-to-bottom.

const BURN_SITES_CAP:    usize = 128;
const SITE_TTL_TICKS:    u32   = 360;
const SITE_LAUNCH_MOD:   u32   = 12;   // 1-in-N chance per site per tick to launch an ember
const EMBERS_CAP:        usize = 64;
const EMBER_TTL_TICKS:   u32   = 240;  // max ticks a single ember stays airborne
// Initial-velocity scales. The (x, z) components are signed in
// [-EMBER_VEL_XZ, +EMBER_VEL_XZ]; the y component is biased upward
// in [EMBER_VEL_Y_MIN, EMBER_VEL_Y_MAX] so embers initially shoot up
// before gravity arcs them back down.
const EMBER_VEL_XZ:       f32 = 0.45;
const EMBER_VEL_Y_MIN:    f32 = 0.55;
const EMBER_VEL_Y_MAX:    f32 = 1.20;
const EMBER_GRAVITY:      f32 = 0.040;

#[derive(Copy, Clone, Default)]
struct Ember {
    active: bool,
    pos:    Vec3,
    vel:    Vec3,
    ttl:    u32,
    /// Last cell the ember was painted into (so we can clear it
    /// before painting the next one). `painted == false` means we
    /// haven't drawn this ember yet (first tick of its life).
    last:   UVec3,
    painted: bool,
}

static mut BURN_SITES: [Option<(UVec3, u32)>; BURN_SITES_CAP] = [None; BURN_SITES_CAP];
static mut EMBERS:     [Ember;                EMBERS_CAP]     = [Ember {
    active: false, pos: Vec3 { x: 0.0, y: 0.0, z: 0.0 }, vel: Vec3 { x: 0.0, y: 0.0, z: 0.0 },
    ttl: 0, last: UVec3 { x: 0, y: 0, z: 0 }, painted: false,
}; EMBERS_CAP];
static mut EMBER_RNG:  u32 = 0xC0FF_EEBA;

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    // ── Materials ─────────────────────────────────────────────
    material_define(M_STONE, Material::pack_color(14, 1), 0, MaterialFlags::empty());
    material_define(M_DIRT,  Material::pack_color( 0, 1), 0, MaterialFlags::empty());
    material_define(M_GRASS, Material::pack_color( 3, 2), 0, MaterialFlags::empty());
    // Wood + leaves are flammable; they ignite into M_FIRE. Wood holds
    // out a bit longer than leaves (higher heat threshold) so the
    // trunk doesn't immediately collapse the moment a leaf catches.
    material_define(
        M_WOOD,
        Material::pack_color( 0, 0), 0,
        MaterialFlags::empty().with(MaterialFlags::FLAMMABLE),
    );
    material_set_ca(M_WOOD, /*threshold*/90, 0, 0, /*ignites_to*/M_FIRE);
    material_define(
        M_LEAF,
        Material::pack_color( 2, 2), 0,
        MaterialFlags::empty().with(MaterialFlags::FLAMMABLE),
    );
    material_set_ca(M_LEAF, /*threshold*/60, 0, 0, /*ignites_to*/M_FIRE);
    material_define(M_SKIN,  Material::pack_color( 1, 3), 0, MaterialFlags::empty());
    material_define(M_SHIRT, Material::pack_color( 7, 2), 0, MaterialFlags::empty());
    // Sign body = warm dark wood; face = bright emissive accent so the
    // letters glow off the front of the slab.
    material_define(M_SIGN_BODY, Material::pack_color( 0, 0), 0, MaterialFlags::empty());
    material_define(M_SIGN_FACE, Material::pack_color(13, 3), 12, MaterialFlags::empty());
    // Bright red glowing reticle voxel — painted each frame at the
    // player's look-at point via the new physics::raycast import.
    material_define(M_RETICLE,   Material::pack_color(10, 3), 14, MaterialFlags::empty());
    // Sand: granular CA flag drives the pile-into-angle-of-repose
    // behavior in §10.3. Color is a warm tan in the Yellow ramp.
    material_define(
        M_SAND,
        Material::pack_color(12, 2),
        0,
        MaterialFlags::empty().with(MaterialFlags::GRANULAR),
    );
    // Water: LIQUID flag drives the lateral-spread CA rule. Color is
    // a saturated cyan so the contrast with the dirt below reads.
    material_define(
        M_WATER,
        Material::pack_color(5, 2),
        0,
        MaterialFlags::empty().with(MaterialFlags::LIQUID),
    );
    // Fire: bright orange + strong emission so it reads against the
    // green forest. ca_lifetime=12 keeps each fire cell short-lived,
    // matching the §10.3 4-bit cap and giving the cascade through a
    // tree a snappy feel.
    material_define(
        M_FIRE,
        Material::pack_color(11, 3),
        14,
        MaterialFlags::empty().with(MaterialFlags::FIRE),
    );
    material_set_ca(M_FIRE, 0, /*lifetime*/15, 0, 0);
    // Ember: bright yellow + max emission, no CA flags. The cart
    // moves these voxels manually each tick (write at new pos, clear
    // at last pos), so they read as little glowing dots arcing
    // through the air without ever entering the §10.3 active set.
    material_define(
        M_EMBER,
        Material::pack_color(12, 3),
        15,
        MaterialFlags::empty(),
    );

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
        ZOOM_ACTION = input_declare_action(ActionKind::Axis1D, BindingHint::Zoom, "zoom");
        FIRE_ACTION = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire, "fire");
    }

    // Boot into the title screen. The world is now fully built; the
    // first fire gets dropped *after* the player presses FIRE and the
    // game scene becomes active (so the burn doesn't tick down
    // invisibly behind the title).
    scene_set_active(SCENE_TITLE);
}

// ── Embers + RNG helpers ──────────────────────────────────────────────────

fn ember_rand() -> u32 {
    unsafe {
        EMBER_RNG = EMBER_RNG
            .wrapping_mul(0x6C8E_9CF5)
            .wrapping_add(0x9E37_79B9);
        EMBER_RNG
    }
}

/// Pseudo-random f32 in [-1, 1].
fn rand_signed() -> f32 {
    let r = ember_rand();
    ((r as i32) as f32) / (i32::MAX as f32)
}

/// Pseudo-random f32 in [0, 1).
fn rand_unit() -> f32 {
    (ember_rand() as f32) / (u32::MAX as f32 + 1.0)
}

fn add_burn_site(pos: UVec3) {
    let sites = unsafe { &mut *(&raw mut BURN_SITES) };
    // Prefer an empty slot; otherwise overwrite the slot with the
    // smallest remaining TTL so a fresher site can take its place.
    let mut worst_idx = 0usize;
    let mut worst_ttl = u32::MAX;
    for (i, slot) in sites.iter_mut().enumerate() {
        match slot {
            None => { *slot = Some((pos, SITE_TTL_TICKS)); return; }
            Some((_, ttl)) => {
                if *ttl < worst_ttl { worst_ttl = *ttl; worst_idx = i; }
            }
        }
    }
    sites[worst_idx] = Some((pos, SITE_TTL_TICKS));
}

fn launch_ember(origin: Vec3) {
    let embers = unsafe { &mut *(&raw mut EMBERS) };
    for e in embers.iter_mut() {
        if e.active { continue; }
        let vx = rand_signed() * EMBER_VEL_XZ;
        let vz = rand_signed() * EMBER_VEL_XZ;
        let vy = EMBER_VEL_Y_MIN + rand_unit() * (EMBER_VEL_Y_MAX - EMBER_VEL_Y_MIN);
        *e = Ember {
            active:  true,
            pos:     origin,
            vel:     Vec3::new(vx, vy, vz),
            ttl:     EMBER_TTL_TICKS,
            last:    UVec3::new(0, 0, 0),
            painted: false,
        };
        return;
    }
    // All slots busy: drop this ember silently.
}

fn seed_first_fire() {
    // Scan a small window above ground near the player's spawn for any
    // M_LEAF or M_WOOD voxel and replace it with M_FIRE. Add the
    // position as the first burn site so embers start radiating.
    let px = unsafe { PLAYER_POS.x } as i32;
    let pz = unsafe { PLAYER_POS.z } as i32;
    for dy in 0..30 {
        for dz in -16i32..=16 {
            for dx in -16i32..=16 {
                let x = (px + dx).clamp(0, WORLD as i32 - 1) as u32;
                let z = (pz + dz).clamp(0, WORLD as i32 - 1) as u32;
                let y = (terrain_height(x, z) as i32 + dy).clamp(0, WORLD as i32 - 1) as u32;
                let m = physics::material_at(x, y, z);
                if m == M_LEAF || m == M_WOOD {
                    set_voxel(UVec3::new(x, y, z), M_FIRE);
                    add_burn_site(UVec3::new(x, y, z));
                    return;
                }
            }
        }
    }
}

/// Clear an ember's previously-painted voxel — but only if it's
/// still our ember marker (sometimes the underlying CA or another
/// rule has already overwritten it).
fn clear_ember_voxel(p: UVec3) {
    if physics::material_at(p.x, p.y, p.z) == M_EMBER {
        set_voxel(p, 0);
    }
}

/// Walk each tracked burn site's 6 cardinal neighbours and add any
/// cell that's now `M_FIRE` (via §10.3 propagation) but not yet
/// tracked. Without this the cart-side sites would die as soon as
/// the original ignition burned out, even though the fire has
/// actually walked into adjacent cells.
fn discover_propagated_fire() {
    const NEIGHBOURS: [(i32, i32, i32); 6] = [
        (-1, 0, 0), (1, 0, 0),
        (0, -1, 0), (0, 1, 0),
        (0, 0, -1), (0, 0, 1),
    ];
    let sites = unsafe { &mut *(&raw mut BURN_SITES) };
    // Snapshot the currently-known positions so we don't pick up our
    // own additions in this pass.
    let mut known: [UVec3; BURN_SITES_CAP] =
        [UVec3 { x: 0, y: 0, z: 0 }; BURN_SITES_CAP];
    let mut known_count = 0usize;
    for slot in sites.iter() {
        if let Some((p, _)) = slot {
            known[known_count] = *p;
            known_count += 1;
        }
    }
    for i in 0..known_count {
        let pos = known[i];
        for &(dx, dy, dz) in &NEIGHBOURS {
            let nx = (pos.x as i32 + dx).clamp(0, WORLD as i32 - 1) as u32;
            let ny = (pos.y as i32 + dy).clamp(0, WORLD as i32 - 1) as u32;
            let nz = (pos.z as i32 + dz).clamp(0, WORLD as i32 - 1) as u32;
            if physics::material_at(nx, ny, nz) != M_FIRE { continue; }
            // Dedup against the snapshot.
            let mut already = false;
            for k in 0..known_count {
                if known[k].x == nx && known[k].y == ny && known[k].z == nz {
                    already = true; break;
                }
            }
            if !already {
                add_burn_site(UVec3::new(nx, ny, nz));
            }
        }
    }
}

fn tick_embers() {
    discover_propagated_fire();

    // ── Phase 1: roll each burn site for a new ember launch. ──
    //
    // A site is only allowed to launch when the cell it points at
    // is *still* M_FIRE. The §10.3 fire rule consumes each cell in
    // ~12 ticks, but the burn site can stay tracked for much
    // longer; we don't want a long tail of embers spawning from a
    // patch of grass that *used to* be on fire.
    let sites = unsafe { &mut *(&raw mut BURN_SITES) };
    for slot in sites.iter_mut() {
        if let Some((pos, ttl)) = *slot {
            // Drop the site the moment the cell stops being fire —
            // the visible flame is gone, so embers shouldn't come
            // from here either.
            if physics::material_at(pos.x, pos.y, pos.z) != M_FIRE {
                *slot = None;
                continue;
            }
            if ttl == 0 { *slot = None; continue; }
            *slot = Some((pos, ttl - 1));
            if ember_rand() % SITE_LAUNCH_MOD != 0 { continue; }
            // Origin = exact burn site, lifted half a cell so the
            // very first paint doesn't fight the active fire voxel.
            let origin = Vec3::new(pos.x as f32 + 0.5, pos.y as f32 + 1.0, pos.z as f32 + 0.5);
            launch_ember(origin);
        }
    }

    // ── Phase 2: step every airborne ember. ──
    let embers = unsafe { &mut *(&raw mut EMBERS) };
    let mut new_sites: [Option<UVec3>; EMBERS_CAP] = [None; EMBERS_CAP];
    let mut new_site_count = 0usize;

    for e in embers.iter_mut() {
        if !e.active { continue; }

        // Clear last-painted cell.
        if e.painted { clear_ember_voxel(e.last); e.painted = false; }

        // TTL: snuff out silently if exceeded.
        if e.ttl == 0 { e.active = false; continue; }
        e.ttl -= 1;

        // Integrate position + velocity.
        e.pos = Vec3::new(e.pos.x + e.vel.x, e.pos.y + e.vel.y, e.pos.z + e.vel.z);
        e.vel = Vec3::new(e.vel.x, e.vel.y - EMBER_GRAVITY, e.vel.z);

        // Snap to integer cell. Clamp to world bounds; if we left
        // the world, drop.
        let xi = e.pos.x as i32;
        let yi = e.pos.y as i32;
        let zi = e.pos.z as i32;
        if xi < 0 || yi < 0 || zi < 0
            || xi >= WORLD as i32 || yi >= WORLD as i32 || zi >= WORLD as i32
        {
            e.active = false;
            continue;
        }
        let cell = UVec3::new(xi as u32, yi as u32, zi as u32);
        let m = physics::material_at(cell.x, cell.y, cell.z);

        // Hit-detection — what's in the new cell?
        if m == M_LEAF || m == M_WOOD {
            // Ignition! Drop a fire voxel and birth a new burn site.
            set_voxel(cell, M_FIRE);
            if new_site_count < new_sites.len() {
                new_sites[new_site_count] = Some(cell);
                new_site_count += 1;
            }
            e.active = false;
            continue;
        }
        if m != 0 && m != M_EMBER {
            // Hit a non-flammable solid (terrain, water, etc.) —
            // snuff the ember.
            e.active = false;
            continue;
        }

        // Empty (or our own previous trail) — paint and continue.
        set_voxel(cell, M_EMBER);
        e.last = cell;
        e.painted = true;
    }

    for i in 0..new_site_count {
        if let Some(p) = new_sites[i] { add_burn_site(p); }
    }
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
            // Light a tree near the player the moment we enter the
            // game scene — the §10.3 fire rule + cart-side embers
            // then carry the burn through the forest.
            seed_first_fire();
        }
        return;
    }

    let dt = (dt_ms as f32) / 1000.0;
    let (mx, my) = input_action_axis2d(unsafe { MOVE_ACTION });
    let (ax, ay) = input_action_axis2d(unsafe { AIM_ACTION });
    let zoom_delta = input_action_axis1d(unsafe { ZOOM_ACTION });

    // ── Camera updates ────────────────────────────────────────
    //
    // Mouse delta drives yaw (left/right) and pitch (up/down) when the
    // browser host has pointer lock — otherwise the host suppresses the
    // delta so the camera stays put while the user reads the page.
    //
    // Wheel scroll drives zoom. Positive `zoom_delta` = scroll-up = zoom
    // in. Step is fraction-of-current-distance per notch so far-away
    // adjustments feel symmetric to close-up ones.
    //
    // Pitch is clamped well short of straight-down (1.20 rad ≈ 69°) and
    // never tilts up past horizontal-plus-a-touch (-0.20 rad ≈ -11°),
    // both to keep the third-person camera readable.
    unsafe {
        CAM_YAW += ax * 0.004;
        // FPS feel: mouse down → look down. Orbit cam sits the eye
        // above the target on positive pitch, so positive `ay` (mouse
        // moved down) maps to increasing pitch.
        CAM_PITCH = (CAM_PITCH + ay * 0.004).clamp(CAM_PITCH_MIN, CAM_PITCH_MAX);
        if zoom_delta != 0.0 {
            CAM_DISTANCE = (CAM_DISTANCE * (1.0 - zoom_delta * ZOOM_PER_NOTCH))
                .clamp(CAM_DISTANCE_MIN, CAM_DISTANCE_MAX);
        }
    }

    let cam_yaw = unsafe { CAM_YAW };
    // forward = where the camera is *looking* (toward target), in the
    // ground plane only — vertical look doesn't affect movement so the
    // dude moves predictably even when the camera is angled steeply.
    let forward = Vec3::new(-sine(cam_yaw), 0.0, -cosine(cam_yaw));
    let right   = Vec3::new(cosine(cam_yaw), 0.0, -sine(cam_yaw));
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

    // ── Targeting reticle ─────────────────────────────────────
    //
    // Demonstrates physics::raycast_world_only (§10.1). We probe a
    // column 6 voxels east + south of the player by casting a ray
    // straight down from y=100 and finding the topmost ground voxel.
    // A 3×3 emissive pad gets painted on top so the orbit cam can
    // see it next to the dude. The previous frame's pad gets cleared
    // first so the marker tracks the player as they move.
    //
    // (The probe-column approach sidesteps the actor-composite issue:
    // a marker painted in the player's column would be hidden behind
    // the dude, since actors render over world voxels in §11.6.)
    unsafe {
        let reticle = &mut *(&raw mut RETICLE_POS);
        if let Some(prev) = reticle.take() {
            fill_box(
                UVec3::new(prev.x.saturating_sub(1), prev.y, prev.z.saturating_sub(1)),
                UVec3::new(prev.x + 1, prev.y, prev.z + 1),
                0,
            );
        }
        let probe_x = (PLAYER_POS.x as u32).saturating_add(6);
        let probe_z = (PLAYER_POS.z as u32).saturating_add(6);
        let probe_origin = Vec3::new(probe_x as f32, 100.0, probe_z as f32);
        let probe_dir = Vec3::new(0.0, -1.0, 0.0);
        if let Some(hit) = physics::raycast_world_only(probe_origin, probe_dir, 200.0) {
            let cx = ((hit.pos.x as i32) + hit.normal.x).clamp(2, 509) as u32;
            let cy = ((hit.pos.y as i32) + hit.normal.y).clamp(2, 509) as u32;
            let cz = ((hit.pos.z as i32) + hit.normal.z).clamp(2, 509) as u32;
            fill_box(
                UVec3::new(cx.saturating_sub(1), cy, cz.saturating_sub(1)),
                UVec3::new(cx + 1, cy, cz + 1),
                M_RETICLE,
            );
            *reticle = Some(UVec3::new(cx, cy, cz));
        }

        // ── Sand + water drops (CA §10.3 demo) ─────────────────
        //
        // Sand drops on the player's east-south side; water on the
        // east-north side. Sand piles at its angle of repose; water
        // equilibrates level-aware (§10.3 liquid state byte) into a
        // flat puddle.
        let drop_y = 60u32;
        SAND_DROP_COUNTER = SAND_DROP_COUNTER.saturating_add(1);
        if SAND_DROP_COUNTER >= SAND_DROP_PERIOD {
            SAND_DROP_COUNTER = 0;
            let sand_x = (PLAYER_POS.x as u32).saturating_add(6);
            let sand_z = (PLAYER_POS.z as u32).saturating_add(6);
            if physics::material_at(sand_x, drop_y, sand_z) == 0 {
                set_voxel(UVec3::new(sand_x, drop_y, sand_z), M_SAND);
            }
        }
        WATER_DROP_COUNTER = WATER_DROP_COUNTER.saturating_add(1);
        if WATER_DROP_COUNTER >= WATER_DROP_PERIOD {
            WATER_DROP_COUNTER = 0;
            let water_x = (PLAYER_POS.x as u32).saturating_add(6);
            let water_z = (PLAYER_POS.z as u32).saturating_sub(6);
            if physics::material_at(water_x, drop_y, water_z) == 0 {
                set_voxel(UVec3::new(water_x, drop_y, water_z), M_WATER);
            }
        }

        // Spread fire via cart-side embers (§10.3 spreads cell-by-cell,
        // far too slow to torch a forest on its own).
        tick_embers();
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
