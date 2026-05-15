//! big-world — voxlconsl's renderer stress test + multi-feature showcase.
//!
//! Builds a 512×512 voxel terrain from cart-side multi-octave value
//! noise, sprinkles ~500 trees on grass tiles, and drops the player
//! down at the centre. The whole point is to populate ~hundreds of
//! chunks across the active scene and see the renderer still hold a
//! sensible frame rate.
//!
//! Memory math (see SPEC.md §13.8):
//! - 512×512 ground × ~10 voxels deep ≈ 2.6 M voxels populated
//! - Across 16×16 = 256 X/Z chunks × 1–2 Y chunks ≈ 256–512 chunks
//! - At ~50 KB/chunk SVO+dense ≈ 12–25 MB resident
//!
//! That fits the spec's ESP32-P4 design point (~25 MB voxel-data
//! budget). On smaller MCUs this cart is honestly out-of-spec and
//! exists only to flex the renderer.
//!
//! ## What this cart demonstrates
//!
//! - **Two-scene cart** — title screen with floating 3D text, then
//!   gameplay scene with a 512³ world.
//! - **Procedural terrain + deterministic tree scatter** —
//!   `terrain::paint_ground` + `terrain::scatter_trees`.
//! - **Flipbook walk-cycle character** with terrain-tracking
//!   movement — `player::tick`.
//! - **§10.1 physics raycast** — targeting reticle that follows the
//!   player around.
//! - **§10.2 rigid bodies** — dynamic AABB crates + sphere balls
//!   fall and settle on the terrain.
//! - **§10.3 cellular automata** — sand pile, level-aware liquid,
//!   plus a cart-driven **ember system** that spreads forest fire
//!   beyond the 1-cell-per-tick CA propagation.
//! - **§5 audio** — boot-loaded patches + samples + SMF song
//!   triggered on scene entry; SPACE plays a sustained lead note,
//!   K plays a kick drum.
//! - **§11.10 text rendering** — title screen baked as 3D voxel
//!   text via `paint_world` with FONT_DCP1 and FONT_ANSI.
//!
//! ## File map
//!
//! | File | What lives there |
//! |---|---|
//! | `lib.rs`     | Entry points, scenes, game state, camera, update orchestration |
//! | `terrain.rs` | Value noise, heightmap → voxels, tree scatter |
//! | `player.rs`  | Prefab frames, walk-cycle flipbook, movement |
//! | `embers.rs`  | Burn sites + airborne embers (forest-fire spread) |
//! | `body_demo.rs` | §10.2 crate stack + leaf-ball demo |
//! | `audio.rs`   | Channel → patch routing + FX bus setup |
//! | `mathlib.rs` | no_std sine / cosine / atan2 |

#![no_std]
#![no_main]

use voxlconsl_sdk::*;
use voxlconsl_sdk::audio as sdk_audio;
use voxlconsl_sdk::audio::DRUM_CHANNEL;
use voxlconsl_sdk::bodies;
use voxlconsl_sdk::physics;
use voxlconsl_sdk::text::{measure, paint_world, Axis, FONT_ANSI, FONT_DCP1};

mod audio;
mod body_demo;
mod embers;
mod mathlib;
mod player;
mod terrain;

use crate::mathlib::{cosine, sine};

// ── World / scenes ───────────────────────────────────────────────

pub(crate) const WORLD: u32 = 512;

const SCENE_TITLE: SceneId = SceneId(0);
const SCENE_GAME:  SceneId = SceneId(1);

#[derive(Copy, Clone, PartialEq, Eq)]
enum GameState { Title, Playing }

static mut STATE: GameState = GameState::Title;
static mut TITLE_CLOCK_MS: u32 = 0;

// ── Materials (slots match materials.toml) ───────────────────────

pub(crate) const M_STONE:     u8 = 1;
pub(crate) const M_DIRT:      u8 = 2;
pub(crate) const M_GRASS:     u8 = 3;
pub(crate) const M_WOOD:      u8 = 4;
pub(crate) const M_LEAF:      u8 = 5;
pub(crate) const M_SKIN:      u8 = 6;
pub(crate) const M_SHIRT:     u8 = 7;
pub(crate) const M_SIGN_BODY: u8 = 8;
pub(crate) const M_SIGN_FACE: u8 = 9;
pub(crate) const M_RETICLE:   u8 = 10;
pub(crate) const M_SAND:      u8 = 11;
pub(crate) const M_WATER:     u8 = 12;
pub(crate) const M_FIRE:      u8 = 13;
pub(crate) const M_EMBER:     u8 = 14;
pub(crate) const M_CRATE:     u8 = 15;

// ── Camera state ─────────────────────────────────────────────────

static mut CAM_YAW:      f32 = 0.7;
static mut CAM_PITCH:    f32 = 0.45;
static mut CAM_DISTANCE: f32 = 28.0;

const CAM_PITCH_MIN:    f32 = -0.20;     // ~-11° (just above looking up)
const CAM_PITCH_MAX:    f32 = 1.20;      // ~+69° (close to top-down)
const CAM_DISTANCE_MIN: f32 = 6.0;
const CAM_DISTANCE_MAX: f32 = 64.0;
/// Fraction of current distance applied per wheel-notch.
const ZOOM_PER_NOTCH:   f32 = 0.12;

// ── Targeting reticle ────────────────────────────────────────────

/// Last-painted reticle voxel position, so we can clear it before
/// painting the next frame's hit point. `None` on first frame.
static mut RETICLE_POS: Option<UVec3> = None;

// ── CA drop counters ─────────────────────────────────────────────

// Frames-per-drop. ~22 fps gameplay × 4 frames ≈ 5 drops/s. Water
// uses the same cadence as sand now that the §10.3 liquid rule tracks
// per-voxel fluid level (v0.1.5).
const SAND_DROP_PERIOD:  u32 = 4;
const WATER_DROP_PERIOD: u32 = 4;
static mut SAND_DROP_COUNTER:  u32 = 0;
static mut WATER_DROP_COUNTER: u32 = 0;

// ── Action handles ───────────────────────────────────────────────

static mut MOVE_ACTION: ActionHandle = ActionHandle(0);
static mut AIM_ACTION:  ActionHandle = ActionHandle(0);
static mut ZOOM_ACTION: ActionHandle = ActionHandle(0);
static mut FIRE_ACTION: ActionHandle = ActionHandle(0);
static mut NOTE_ACTION: ActionHandle = ActionHandle(0);
static mut KICK_ACTION: ActionHandle = ActionHandle(0);

// ── Boot ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    // Materials live in materials.toml — the bundler emits them into
    // the cart's Materials section and the host pre-populates the
    // table before this function runs (SPEC.md §7).

    sky_set_gradient(Material::pack_color(7, 0), Material::pack_color(6, 0));
    light_set_sun(Vec3::new(-0.6, 0.8, 0.4), 0, 0);

    // Earth-ish gravity — voxlconsl voxels are dimensionless, but
    // ~10 units/sec² along -Y feels right for "1 voxel ~ 1 meter"
    // scale and makes the crates settle in roughly half a second.
    bodies::world_set_gravity(Vec3::new(0.0, -10.0, 0.0));

    // The cart owns two scenes: a clean void where the title text
    // floats (scene 0) and the gameplay world below (scene 1). We
    // build scene 1 first, then scene 0, leaving 0 active so the
    // cart boots into the title.
    scene_set_active(SCENE_GAME);

    terrain::paint_ground();
    terrain::scatter_trees();

    paint_title_scene();

    // Back to the gameplay scene to define the player prefab and
    // spawn the actor; the title scene stays clean of game-world data.
    scene_set_active(SCENE_GAME);
    player::init();

    register_inputs();
    audio::configure();

    // Boot into the title screen. The world is now fully built; the
    // first fire gets dropped *after* the player presses FIRE and the
    // game scene becomes active (so the burn doesn't tick down
    // invisibly behind the title).
    scene_set_active(SCENE_TITLE);
}

fn register_inputs() {
    unsafe {
        MOVE_ACTION = input_declare_action(ActionKind::Axis2D, BindingHint::PrimaryMovement, "move");
        AIM_ACTION  = input_declare_action(ActionKind::Axis2D, BindingHint::Aim, "aim");
        ZOOM_ACTION = input_declare_action(ActionKind::Axis1D, BindingHint::Zoom, "zoom");
        FIRE_ACTION = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire, "fire");
        // `BindingHint::None` → Space on the browser port.
        NOTE_ACTION = input_declare_action(ActionKind::Button, BindingHint::None, "note");
        // SecondaryFire → K on the browser port.
        KICK_ACTION = input_declare_action(ActionKind::Button, BindingHint::SecondaryFire, "kick");
    }
}

/// Paint the floating 3D title text into the title scene. Switches
/// the active scene to SCENE_TITLE for the paint and leaves it that
/// way; the caller switches back to SCENE_GAME after the player
/// prefab is registered.
fn paint_title_scene() {
    // Title text uses FONT_DCP1 (16×18 chiseled-serif). The subtitle
    // uses FONT_ANSI for the smaller "PRESS FIRE" line. Both go in
    // the XY plane, so the +Z face is what the orbit camera reads
    // when it passes through cam_yaw == 0.
    //
    // To put the emissive face on the +Z side (the side the camera
    // sees from cam_yaw≈0), we pass the dark body material as
    // face_color and the bright face material as the main color —
    // the spec's documented front/back swap.
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
        2,         // 2× scale → 32×36 voxel letters
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
}

// ── Per-frame update ─────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    // Title-screen state: orbit camera + wait for FIRE to start the game.
    if unsafe { STATE } == GameState::Title {
        unsafe { TITLE_CLOCK_MS = TITLE_CLOCK_MS.saturating_add(dt_ms); }
        if input_action_pressed(unsafe { FIRE_ACTION }) {
            enter_gameplay();
        }
        return;
    }

    let dt = (dt_ms as f32) / 1000.0;
    let (mx, my)   = input_action_axis2d(unsafe { MOVE_ACTION });
    let (ax, ay)   = input_action_axis2d(unsafe { AIM_ACTION });
    let zoom_delta = input_action_axis1d(unsafe { ZOOM_ACTION });

    handle_audio_triggers();
    update_camera_state(ax, ay, zoom_delta);

    let cam_yaw = unsafe { CAM_YAW };
    player::tick(mx, my, cam_yaw, dt, dt_ms);

    update_reticle();
    drop_sand_and_water();
    embers::tick();
}

fn handle_audio_triggers() {
    // SPACE → sustained synth note on channel 0. Hold to sustain at
    // the patch's sustain level; release for a fade-out + auto-free.
    // We go through the MIDI surface so `note_off` finds the right
    // voice by (channel, note) — no need to track VoiceIds.
    if input_action_pressed(unsafe { NOTE_ACTION }) {
        let _ = sdk_audio::note_on(/*channel*/0, audio::SYNTH_NOTE, /*velocity*/110);
    }
    if input_action_released(unsafe { NOTE_ACTION }) {
        sdk_audio::note_off(/*channel*/0, audio::SYNTH_NOTE);
    }

    // K → kick drum on channel 10 (DRUM_CHANNEL). GM note 36 = Bass
    // Drum 1 in the boot-synthesized drum kit. One-shot — drum voices
    // auto-free at sample end.
    if input_action_pressed(unsafe { KICK_ACTION }) {
        let _ = sdk_audio::note_on(DRUM_CHANNEL, /*GM kick*/36, /*velocity*/110);
    }
}

fn update_camera_state(ax: f32, ay: f32, zoom_delta: f32) {
    // Mouse delta drives yaw (left/right) and pitch (up/down) when
    // the browser host has pointer lock — otherwise the host
    // suppresses the delta so the camera stays put.
    //
    // Wheel scroll drives zoom. Positive `zoom_delta` = scroll-up
    // = zoom in. Step is fraction-of-current-distance per notch so
    // far-away adjustments feel symmetric to close-up ones.
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
}

/// Cast a ray straight down 6 voxels east + south of the player and
/// paint a 3×3 emissive pad on top of whatever it hits. The pad
/// follows the player as they move — old pad is cleared each frame
/// before the new one paints. Demonstrates `physics::raycast_world_only`.
///
/// The probe-column approach sidesteps the actor-composite issue: a
/// marker painted in the player's own column would be hidden behind
/// the dude since actors render over world voxels in §11.6.
fn update_reticle() {
    unsafe {
        let reticle = &mut *(&raw mut RETICLE_POS);
        if let Some(prev) = reticle.take() {
            fill_box(
                UVec3::new(prev.x.saturating_sub(1), prev.y, prev.z.saturating_sub(1)),
                UVec3::new(prev.x + 1, prev.y, prev.z + 1),
                0,
            );
        }
        let probe_x = (player::PLAYER_POS.x as u32).saturating_add(6);
        let probe_z = (player::PLAYER_POS.z as u32).saturating_add(6);
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
    }
}

/// Drop one sand voxel east-south of the player every
/// `SAND_DROP_PERIOD` frames, and one water voxel east-north every
/// `WATER_DROP_PERIOD` frames. Sand piles at its angle of repose;
/// water equilibrates level-aware (§10.3 liquid state byte) into a
/// flat puddle.
fn drop_sand_and_water() {
    unsafe {
        let drop_y = 60u32;
        SAND_DROP_COUNTER = SAND_DROP_COUNTER.saturating_add(1);
        if SAND_DROP_COUNTER >= SAND_DROP_PERIOD {
            SAND_DROP_COUNTER = 0;
            let sand_x = (player::PLAYER_POS.x as u32).saturating_add(6);
            let sand_z = (player::PLAYER_POS.z as u32).saturating_add(6);
            if physics::material_at(sand_x, drop_y, sand_z) == 0 {
                set_voxel(UVec3::new(sand_x, drop_y, sand_z), M_SAND);
            }
        }
        WATER_DROP_COUNTER = WATER_DROP_COUNTER.saturating_add(1);
        if WATER_DROP_COUNTER >= WATER_DROP_PERIOD {
            WATER_DROP_COUNTER = 0;
            let water_x = (player::PLAYER_POS.x as u32).saturating_add(6);
            let water_z = (player::PLAYER_POS.z as u32).saturating_sub(6);
            if physics::material_at(water_x, drop_y, water_z) == 0 {
                set_voxel(UVec3::new(water_x, drop_y, water_z), M_WATER);
            }
        }
    }
}

/// Transition from title screen to gameplay. Seeds the first fire,
/// drops the body-physics stack, kicks off music — everything that
/// should only happen once we're committed to the game scene.
fn enter_gameplay() {
    unsafe { STATE = GameState::Playing; }
    player::make_visible();
    scene_set_active(SCENE_GAME);
    embers::seed_first_fire();
    body_demo::spawn_demo_stack();
    // The §10.3 fire crackle + drums + saw lead all share the same
    // voice pool — voice stealing keeps polyphony honest under load.
    sdk_audio::music_play(0, /*loop_*/true);
}

// ── Render ───────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    if unsafe { STATE } == GameState::Title {
        render_title_camera();
        return;
    }
    render_orbit_camera();
}

/// Title screen orbit camera. Sways yaw within ±15° instead of
/// full-orbit so the camera always looks at the title's emissive
/// `+Z` face (full orbit would show edge-on ribs).
fn render_title_camera() {
    let t = unsafe { TITLE_CLOCK_MS } as f32 / 1000.0;
    let yaw = sine(t * 0.4) * 0.26;
    // Subtitle sits 20 voxels below the title (y≈218). Target a point
    // between them so the vertical FOV frames both.
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
}

/// Gameplay orbit camera around the player.
fn render_orbit_camera() {
    let (yaw, pitch, dist) = unsafe { (CAM_YAW, CAM_PITCH, CAM_DISTANCE) };
    let pos = unsafe { player::PLAYER_POS };

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

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log("big-world cart panicked");
    let _ = info;
    loop {}
}
