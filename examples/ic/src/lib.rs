//! **ic — Incident Command.** An RTS wildfire sim where you direct a
//! crew + helicopter to protect a small town from an advancing
//! forest fire.
//!
//! ## What this cart demonstrates
//!
//! - **Tilted-overhead RTS camera** with WASD pan + wheel zoom on a
//!   perspective renderer (no orthographic projection required).
//! - **Cart-emulated cursor** driven by Aim-hint mouse-deltas — the
//!   §6.4 pointer API is still TBD, so we paint a glowing reticle
//!   into the world and step it with the same axis the FPS camera
//!   uses.
//! - **§10.3 CA + cart-side ember layer** working in concert: the
//!   engine spreads fire cell-by-cell, the cart launches airborne
//!   embers with wind drift toward the town, and crews lay
//!   non-flammable firebreaks that snuff embers on landing.
//! - **Multiple unit types as actors** with state machines (heli
//!   refill cycle, ground crew bulldozer path) and a click-to-select
//!   / click-to-order command model.
//! - **Voxel-bar HUD** painted into the world — the camera focus
//!   anchors a shrinking timer bar + per-cabin survival dots high
//!   above the terrain so they're always in view.
//!
//! ## File map
//!
//! | File | What lives there |
//! |---|---|
//! | `lib.rs`     | Entry points + game state + input + win/lose |
//! | `terrain.rs` | Heightmap, lake, town, roads, fire-seed picker |
//! | `camera.rs`  | RTS camera state + apply() |
//! | `cursor.rs`  | World-voxel cursor reticle |
//! | `fire.rs`    | Burn sites + airborne embers + wind |
//! | `units.rs`   | Helicopter + ground crew + roster + orders |
//! | `hud.rs`     | Timer bar, structure dots, banner text |
//! | `mathlib.rs` | no_std sin/cos/sqrt |
//! | `rng.rs`     | xorshift32 |

#![no_std]
#![no_main]

use voxlconsl_sdk::*;

mod camera;
mod cursor;
mod fire;
mod hud;
mod line_mode;
mod mathlib;
mod queue_markers;
mod rng;
mod scenario;
mod terrain;
mod units;

// ── Material slots (mirror materials.toml) ───────────────────────

pub(crate) const M_STONE:            u8 = 1;
pub(crate) const M_DIRT:             u8 = 2;
pub(crate) const M_GRASS:            u8 = 3;
pub(crate) const M_PINE_WOOD:        u8 = 4;
pub(crate) const M_PINE_LEAVES:      u8 = 5;
pub(crate) const M_WATER:            u8 = 6;
pub(crate) const M_FIRE:             u8 = 7;
pub(crate) const M_EMBER:            u8 = 8;
pub(crate) const M_CABIN_WOOD:       u8 = 9;
pub(crate) const M_CABIN_ROOF:       u8 = 10;
pub(crate) const M_ROAD_DIRT:        u8 = 11;
pub(crate) const M_FIREBREAK_DIRT:   u8 = 12;
pub(crate) const M_CURSOR_MARKER:    u8 = 13;
pub(crate) const M_SELECT_MARKER:    u8 = 14;
pub(crate) const M_HELI_PAD:         u8 = 15;
pub(crate) const M_HELICOPTER_BODY:  u8 = 16;
pub(crate) const M_HELICOPTER_ROTOR: u8 = 17;
pub(crate) const M_CREW_BODY:        u8 = 18;
pub(crate) const M_CREW_HELMET:      u8 = 19;
pub(crate) const M_BUCKET_WATER:     u8 = 20;
pub(crate) const M_HUD_TEXT:         u8 = 21;
pub(crate) const M_PLANNED_LINE:     u8 = 25;

// ── Mission tuning ────────────────────────────────────────────────

const MISSION_DURATION_MS: u32 = 180_000;       // 3:00
const WIN_STRUCTURE_THRESHOLD: u32 = 4;         // need 4 of 6 alive at expiry

/// Scenario seed for this build. Change to roll a new map — same
/// seed always reproduces the same forest pattern and starting wind.
/// Future work: surface as a URL param / cart arg.
const MISSION_SEED: u32 = 0xA1F0_5E57;

/// Difficulty tier (1..). Tier 1 = crews only; tier 2 unlocks the
/// helicopter; tier 3 doubles up. See `Scenario::budget_for_tier`
/// for the full table.
const MISSION_TIER: u8 = 2;

// ── Game state ───────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq)]
enum Phase {
    Playing,
    Won,
    Lost,
}

static mut PHASE: Phase = Phase::Playing;
static mut TIME_LEFT_MS: u32 = MISSION_DURATION_MS;
static mut FIRE_SITES_LAST: u32 = 0;

static mut CAMERA: camera::Camera = camera::Camera::new(0.0, 0.0);
static mut CURSOR: cursor::Cursor = cursor::Cursor::new(0.0, 0.0);
static mut FIRE_STATE: fire::FireState = fire::FireState::new();
static mut HUD: hud::Hud = hud::Hud::new();
static mut ROSTER: Option<units::Roster> = None;

// ── Action handles ───────────────────────────────────────────────

static mut PAN_ACTION:    ActionHandle = ActionHandle(0);
static mut AIM_ACTION:    ActionHandle = ActionHandle(0);
static mut ZOOM_ACTION:   ActionHandle = ActionHandle(0);
/// PRIMARY (J) — queue a water drop at the cursor cell.
static mut DROP_ACTION:   ActionHandle = ActionHandle(0);
/// SECONDARY (K) — append a fire-line point at the cursor cell.
/// Press repeatedly to draft up to LINE_CAP points.
static mut LINE_ACTION:   ActionHandle = ActionHandle(0);
/// CONFIRM (Enter) — commit the current fire-line draft into the
/// queue. Empty drafts no-op.
static mut COMMIT_ACTION: ActionHandle = ActionHandle(0);
/// CANCEL (Esc) — discard the current fire-line draft.
static mut CANCEL_ACTION: ActionHandle = ActionHandle(0);

static mut LINE_MODE: line_mode::LineMode = line_mode::LineMode::new();
static mut QUEUE_MARKERS: queue_markers::QueueMarkers = queue_markers::QueueMarkers::new();

// ── Boot ─────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    // Smoky-blue sky. Top is a hazy dusk, horizon is lighter.
    sky_set_gradient(
        Material::pack_color(7, 1),
        Material::pack_color(6, 2),
    );
    // Sun coming in from the south-west, low angle — long shadows
    // give the terrain depth from the high overhead view.
    light_set_sun(Vec3::new(-0.5, -0.6, 0.6), 0, 0);

    // Pick a scenario seed + tier and lock them in before any
    // world / fire / roster initialisation reads from them.
    scenario::init(MISSION_SEED, MISSION_TIER);

    // Reserve the left 36 pixels of the framebuffer for the
    // sidebar: the world ray-march skips that strip entirely.
    // World viewport = (36, 0, 220, 144).
    viewport_set(36, 0, 220, 144);

    terrain::paint_world();

    let focus_x = terrain::HELI_PAD_X as f32;
    let focus_z = terrain::HELI_PAD_Z as f32 - 20.0;
    unsafe {
        CAMERA = camera::Camera::new(focus_x, focus_z);
        CURSOR = cursor::Cursor::new(focus_x, focus_z - 8.0);
        let s = scenario::get();
        ROSTER = Some(units::Roster::init(s.heli_count, s.crew_count));
        register_actions();
        (&mut *(&raw mut HUD)).init();
        (&mut *(&raw mut CURSOR)).init();
        (&mut *(&raw mut LINE_MODE)).init();
        (&mut *(&raw mut QUEUE_MARKERS)).init();
    }

    // First fire — seed it now so the player sees smoke from the
    // opening moment.
    let fire_seed = terrain::ignite_first_fire();
    unsafe {
        let fire = &mut *(&raw mut FIRE_STATE);
        fire.apply_scenario(scenario::get());
        fire.add_burn_site(fire_seed);
    }
}

fn register_actions() {
    unsafe {
        PAN_ACTION    = input_declare_action(ActionKind::Axis2D, BindingHint::PrimaryMovement, "pan");
        AIM_ACTION    = input_declare_action(ActionKind::Axis2D, BindingHint::Aim, "cursor");
        ZOOM_ACTION   = input_declare_action(ActionKind::Axis1D, BindingHint::Zoom, "zoom");
        DROP_ACTION   = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire, "drop");
        LINE_ACTION   = input_declare_action(ActionKind::Button, BindingHint::SecondaryFire, "line_point");
        COMMIT_ACTION = input_declare_action(ActionKind::Button, BindingHint::Confirm, "commit");
        CANCEL_ACTION = input_declare_action(ActionKind::Button, BindingHint::Cancel, "cancel");
    }
}

// ── Per-frame update ─────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    let dt = (dt_ms as f32) / 1000.0;

    // Read input axes once per frame — sandbox edge events
    // (`_pressed`) only return true the frame the press landed.
    let (mx, my) = input_action_axis2d(unsafe { PAN_ACTION });
    let (ax, ay) = input_action_axis2d(unsafe { AIM_ACTION });
    let zoom = input_action_axis1d(unsafe { ZOOM_ACTION });

    let cam = unsafe { &mut *(&raw mut CAMERA) };
    let cur = unsafe { &mut *(&raw mut CURSOR) };
    cam.update(mx, my, zoom, dt);
    cur.pan(ax, ay, cam.cursor_speed());

    // Input edges. Reading once per frame keeps the cart deterministic
    // regardless of how many times `_pressed` would echo.
    let pressed_drop   = input_action_pressed(unsafe { DROP_ACTION });
    let pressed_line   = input_action_pressed(unsafe { LINE_ACTION });
    let pressed_commit = input_action_pressed(unsafe { COMMIT_ACTION });
    let pressed_cancel = input_action_pressed(unsafe { CANCEL_ACTION });

    let phase = unsafe { PHASE };
    if phase == Phase::Playing {
        // PRIMARY (J) — queue a water drop at the cursor cell.
        // Always: the heli's order queue is independent of any
        // fire-line draft.
        if pressed_drop {
            let (cx, cz) = cur.cell();
            let cy = cur.marker_y();
            if let Some(roster) = unsafe { &mut *(&raw mut ROSTER) } {
                roster.dispatch_water_drop(UVec3::new(cx, cy, cz));
            }
        }

        // SECONDARY (K) — append a fire-line point at the cursor cell.
        // Each press adds one point; the player taps K repeatedly
        // while panning the cursor to draft a polyline.
        if pressed_line {
            let (cx, cz) = cur.cell();
            let cy = cur.marker_y();
            unsafe {
                (&mut *(&raw mut LINE_MODE)).push_point(UVec3::new(cx, cy, cz));
            }
        }

        // CONFIRM (Enter) — commit the current fire-line draft.
        // Empty drafts no-op.
        if pressed_commit {
            unsafe {
                let lm = &mut *(&raw mut LINE_MODE);
                if lm.count > 0 {
                    let mut buf = [UVec3::ZERO; line_mode::LINE_CAP];
                    let n = lm.copy_points_into(&mut buf);
                    if let Some(roster) = &mut *(&raw mut ROSTER) {
                        roster.dispatch_fire_line(&buf[..n]);
                    }
                    lm.clear();
                }
            }
        }

        // CANCEL (Esc) — discard the current draft without queueing.
        if pressed_cancel {
            unsafe { (&mut *(&raw mut LINE_MODE)).clear(); }
        }

        unsafe {
            let fire = &mut *(&raw mut FIRE_STATE);
            fire.tick();
            FIRE_SITES_LAST = fire.burn_site_count();
            if let Some(roster) = &mut *(&raw mut ROSTER) {
                roster.tick();
                let markers = &mut *(&raw mut QUEUE_MARKERS);
                markers.update(&roster.queue);
            }
        }

        // Mission time — saturating so we don't underflow on a
        // long renderer stall.
        unsafe { TIME_LEFT_MS = TIME_LEFT_MS.saturating_sub(dt_ms); }
        check_end_conditions();
    }

    // Cursor render is last so it sits on top of any fresh
    // ember / water / firebreak voxels painted this frame.
    cur.render();

    // HUD paints regardless of phase so the player can see the
    // final timer + dot state on the end screen.
    let alive_mask = surviving_mask();
    let ctx = unsafe { build_hud_ctx(alive_mask) };
    unsafe { (&mut *(&raw mut HUD)).paint(ctx); }
}

unsafe fn build_hud_ctx(alive_mask: u32) -> hud::HudCtx<'static> {
    let roster_ref = unsafe { &*(&raw const ROSTER) };
    let fire_ref   = unsafe { &*(&raw const FIRE_STATE) };
    let line_ref   = unsafe { &*(&raw const LINE_MODE) };
    let s          = scenario::get();

    let wind_dir = fire_ref.wind_direction_label();
    let wind_strength = fire_ref.wind_strength_digit();

    let (heli_busy, heli_total, crew_busy, crew_total, queue_total) =
        match roster_ref {
            Some(r) => (
                r.heli_busy(), r.heli_total(),
                r.crew_busy(), r.crew_total(),
                r.queue.pending_total(),
            ),
            None => (0, 0, 0, 0, 0),
        };

    hud::HudCtx {
        time_left_ms: unsafe { TIME_LEFT_MS },
        alive_mask,
        fire_sites:   unsafe { FIRE_SITES_LAST },
        wind_dir,
        wind_strength,
        heli_busy,
        heli_total,
        crew_busy,
        crew_total,
        tier: s.tier as u32,
        line_mode_active: line_ref.is_drafting(),
        line_mode_count:  line_ref.count as u32,
        queue_total,
    }
}

// ── Render ───────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    unsafe { (&*(&raw const CAMERA)).apply(); }
}

// ── Win / lose ───────────────────────────────────────────────────

fn surviving_mask() -> u32 {
    use voxlconsl_sdk::physics;
    // Re-scan each cabin. Same threshold as
    // `terrain::count_surviving_cabins`, but per-cabin so we can
    // set the corresponding HUD-dot bit individually.
    let mut mask = 0u32;
    for (i, &(cx, cz)) in terrain::CABINS.iter().enumerate() {
        let base = terrain::terrain_height(cx, cz);
        const CABIN_SX: u32 = 7;
        const CABIN_SZ: u32 = 6;
        const CABIN_H:  u32 = 7;            // walls (4) + roof (2) + foundation (1)
        let total = CABIN_SX * CABIN_SZ * CABIN_H;
        let mut count = 0u32;
        for y in base..base + CABIN_H {
            for dz in 0..CABIN_SZ {
                for dx in 0..CABIN_SX {
                    let m = physics::material_at(cx + dx, y, cz + dz);
                    if m == M_CABIN_WOOD || m == M_CABIN_ROOF { count += 1; }
                }
            }
        }
        if count * 3 >= total { mask |= 1 << i; }
    }
    mask
}

fn check_end_conditions() {
    let alive = surviving_mask().count_ones();
    let time_left = unsafe { TIME_LEFT_MS };
    if alive == 0 {
        unsafe { PHASE = Phase::Lost; }
        return;
    }
    if time_left == 0 {
        unsafe {
            PHASE = if alive >= WIN_STRUCTURE_THRESHOLD {
                Phase::Won
            } else {
                Phase::Lost
            };
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    log("ic cart panicked");
    loop {}
}
