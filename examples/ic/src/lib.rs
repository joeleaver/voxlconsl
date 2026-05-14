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

mod action_wheel;
mod balance;
mod camera;
mod cursor;
mod engine;
mod fire;
mod hotshot;
mod hud;
mod line_mode;
mod mathlib;
mod queue_markers;
mod retardant_aim;
mod rng;
mod scenario;
mod season;
mod story;
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
pub(crate) const M_PLANNED_WATER:    u8 = 26;
pub(crate) const M_RETARDANT:        u8 = 27;
pub(crate) const M_PLANNED_RETARDANT: u8 = 28;
pub(crate) const M_TANKER_BODY:      u8 = 29;
pub(crate) const M_TANKER_WING:      u8 = 30;
pub(crate) const M_TANKER_WATER_STRIPE: u8 = 31;
pub(crate) const M_TANKER_RETARDANT_STRIPE: u8 = 32;
pub(crate) const M_PARACHUTE:        u8 = 33;
pub(crate) const M_HOTSHOT_STRIPE:   u8 = 34;
pub(crate) const M_HOTSHOT_BODY:     u8 = 35;
pub(crate) const M_HOTSHOT_HELMET:   u8 = 36;
pub(crate) const M_PLANNED_HOTSHOT:  u8 = 37;
pub(crate) const M_ENGINE_BODY:      u8 = 38;
pub(crate) const M_ENGINE_HOSE:      u8 = 39;

// ── Season tuning ────────────────────────────────────────────────
//
// A "season" is the top-level gameplay loop: `DAYS_PER_SEASON` days,
// each `DAY_DURATION_MS` long, lightning strikes during each day.
// See `season.rs` for the constants and state machine.

/// Scenario seed for this build. Change to roll a new map — same
/// seed always reproduces the same forest pattern and weather.
/// Future work: surface as a URL param / cart arg.
const MISSION_SEED: u32 = 0xA1F0_5E57;

/// Difficulty tier (1..). Tier 1 = crews only; tier 2 unlocks the
/// helicopter; tier 3 doubles up. See `Scenario::budget_for_tier`
/// for the full table.
const MISSION_TIER: u8 = 2;

/// Story-mode level selector. `Some(N)` boots into `story::LEVELS[N]`
/// (seed/tier/days from the level table). `None` plays endless mode
/// from `MISSION_SEED` + `MISSION_TIER` + `DAYS_PER_SEASON`.
///
/// This is the placeholder mechanism while the in-game level selector
/// hasn't been built — set the const here at compile time and rebuild.
const STORY_LEVEL: Option<usize> = None;

/// Runtime balance-mode flag. Set true by `init()` when an external
/// harness (e.g. `voxlconsl balance`) supplies a scenario override
/// via `balance_get_scenario_override()`. While true, the cart logs
/// CSV-formatted rows via `log()` every
/// `balance::BALANCE_LOG_INTERVAL_MS` of mission time and suppresses
/// all player input so the run captures a no-op-player baseline.
/// Normal cart boots (no override) leave this false; nothing changes.
static mut BALANCE_MODE_FLAG: bool = false;

#[inline]
fn balance_mode() -> bool { unsafe { BALANCE_MODE_FLAG } }

// ── Game state ───────────────────────────────────────────────────

// `Phase` was the per-mission state machine — replaced by
// `season::SeasonState` now that play runs across multiple days.
// SEASON is initialised in init() once we know the seed; we use
// MaybeUninit so the static can hold a real value without a const
// constructor.

static mut FIRE_SITES_LAST: u32 = 0;
static mut BAL: balance::BalanceLog = balance::BalanceLog::new();
static mut SEASON: Option<season::Season> = None;

static mut CAMERA: camera::Camera = camera::Camera::new(0.0, 0.0);
static mut CURSOR: cursor::Cursor = cursor::Cursor::new(0.0, 0.0);
static mut FIRE_STATE: fire::FireState = fire::FireState::new();
static mut HUD: hud::Hud = hud::Hud::new();
static mut ROSTER: Option<units::Roster> = None;

// ── Action handles ───────────────────────────────────────────────

static mut PAN_ACTION:      ActionHandle = ActionHandle(0);
static mut AIM_ACTION:      ActionHandle = ActionHandle(0);
static mut ZOOM_ACTION:     ActionHandle = ActionHandle(0);
/// "Do the thing at this cell" (`BindingHint::PrimaryFire`).
/// Idle: open the action wheel anchored at the cursor cell.
/// WheelOpen: commit the highlighted option (same as Confirm).
/// LineDrafting: append the next fire-line point.
static mut PRIMARY_ACTION:  ActionHandle = ActionHandle(0);
/// "Yes, do it" (`BindingHint::Confirm`). WheelOpen: commit the
/// highlighted option. LineDrafting: commit the draft to the queue.
/// Idle: no-op.
static mut CONFIRM_ACTION:  ActionHandle = ActionHandle(0);
/// "Back out" (`BindingHint::Cancel`). WheelOpen: close the wheel.
/// LineDrafting: discard the draft. Idle: no-op.
static mut CANCEL_ACTION:   ActionHandle = ActionHandle(0);

static mut LINE_MODE: line_mode::LineMode = line_mode::LineMode::new();
static mut QUEUE_MARKERS: queue_markers::QueueMarkers = queue_markers::QueueMarkers::new();
static mut ACTION_WHEEL: action_wheel::ActionWheel = action_wheel::ActionWheel::new();
static mut RETARDANT_AIM: retardant_aim::RetardantAim = retardant_aim::RetardantAim::new();
static mut INTERACTION: InteractionMode = InteractionMode::Idle;
/// Previous frame's pan axis sample. Used to derive edge presses for
/// W / S while the action wheel is open — the engine only gives us a
/// continuous Axis2D for movement, not per-key edges.
static mut PAN_PREV: (f32, f32) = (0.0, 0.0);
/// Threshold for treating a pan-axis component as a "pressed"
/// direction. 0.5 is well past any analog drift but doesn't require
/// the player to slam the key.
const NAV_THRESHOLD: f32 = 0.5;

#[derive(Copy, Clone, PartialEq, Eq)]
enum Drafter {
    /// Line will be dispatched to the firetruck queue.
    Truck,
    /// Line will be dispatched to the hot-shot queue (parachute deploy).
    HotShot,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum InteractionMode {
    /// No order in progress. Primary opens the action wheel.
    Idle,
    /// Wheel open, anchored at a captured cursor cell. Nav-up /
    /// nav-down move the highlight, Confirm or Primary commits the
    /// pick, Cancel closes.
    WheelOpen,
    /// Fire-line / hot-shot draft underway. Primary appends the next
    /// point at the current cursor cell, Confirm commits, Cancel
    /// discards. The `Drafter` distinguishes truck-laid vs hot-shot
    /// deploys; the UI shares the line_mode preview either way.
    LineDrafting(Drafter),
    /// Retardant aim mode after picking RETARDANT from the wheel.
    /// Only the direction is player-controlled — the strip is a
    /// fixed length from the wheel anchor toward the cursor.
    /// Confirm or Primary paints, Cancel discards.
    RetardantAiming,
}

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
    // world / fire / roster initialisation reads from them. Priority:
    //   1. Balance-harness override (CLI `voxlconsl balance`): forces
    //      a specific seed+tier and turns on BALANCE_MODE_FLAG.
    //   2. Story-mode level (compile-time `STORY_LEVEL` const).
    //   3. Endless mode (compile-time `MISSION_SEED` + `MISSION_TIER`).
    let (mission_seed, mission_tier, season_days) = match balance_get_scenario_override() {
        Some((seed, tier)) => {
            unsafe { BALANCE_MODE_FLAG = true; }
            (seed, tier, season::DAYS_PER_SEASON)
        }
        None => match STORY_LEVEL.and_then(|i| story::LEVELS.get(i)) {
            Some(level) => (level.seed, level.tier, level.days),
            None        => (MISSION_SEED, MISSION_TIER, season::DAYS_PER_SEASON),
        },
    };
    scenario::init(mission_seed, mission_tier);

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
        ROSTER = Some(units::Roster::init(
            s.heli_count, s.crew_count, s.hotshot_count, s.engine_count,
        ));
        register_actions();
        (&mut *(&raw mut HUD)).init();
        (&mut *(&raw mut CURSOR)).init();
        (&mut *(&raw mut LINE_MODE)).init();
        (&mut *(&raw mut QUEUE_MARKERS)).init();
        (&mut *(&raw mut ACTION_WHEEL)).init();
        units::init_tanker_prefabs();
        hotshot::init_drop_plane_prefab();
        hotshot::init_scatter_rng(s.seed);
    }

    unsafe {
        // Season state machine — drives day timer + lightning strikes
        // from now on. No initial fire seed; the first lightning of
        // day 1 provides the first incident. Day count comes from the
        // story-mode level table when STORY_LEVEL is set.
        SEASON = Some(season::Season::new_with_days(mission_seed, season_days));
        let fire = &mut *(&raw mut FIRE_STATE);
        if let Some(s) = &*(&raw const SEASON) {
            fire.set_wind(s.weather.angle_rad, s.weather.strength);
        }
    }

    if balance_mode() {
        unsafe { (&*(&raw const BAL)).emit_header(); }
    }
}

fn register_actions() {
    unsafe {
        PAN_ACTION     = input_declare_action(ActionKind::Axis2D, BindingHint::PrimaryMovement, "pan");
        AIM_ACTION     = input_declare_action(ActionKind::Axis2D, BindingHint::Aim,             "cursor");
        ZOOM_ACTION    = input_declare_action(ActionKind::Axis1D, BindingHint::Zoom,            "zoom");
        PRIMARY_ACTION = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire,     "primary");
        CONFIRM_ACTION = input_declare_action(ActionKind::Button, BindingHint::Confirm,         "confirm");
        CANCEL_ACTION  = input_declare_action(ActionKind::Button, BindingHint::Cancel,          "cancel");
    }
}

/// Dispatch primary / confirm / cancel + nav-up / nav-down edges
/// through the interaction state machine. Each button has the same
/// meaning across modes:
///
/// - **Primary** — "click the world" (opens the wheel, appends a
///   line point, or re-anchors an already-open wheel).
/// - **Confirm** — "yes, do it" (commits a wheel pick or a fire-line
///   draft).
/// - **Cancel** — "back out" (closes the wheel, discards a draft).
unsafe fn handle_interaction(
    cursor_cell: UVec3,
    primary: bool, confirm: bool, cancel: bool,
    nav_up: bool, nav_down: bool,
) {
    let mode = unsafe { INTERACTION };
    let lm     = unsafe { &mut *(&raw mut LINE_MODE) };
    let wheel  = unsafe { &mut *(&raw mut ACTION_WHEEL) };
    let aim    = unsafe { &mut *(&raw mut RETARDANT_AIM) };
    let roster = unsafe { &mut *(&raw mut ROSTER) };

    match mode {
        InteractionMode::Idle => {
            if primary {
                wheel.open_at(cursor_cell);
                unsafe { INTERACTION = InteractionMode::WheelOpen; }
            }
        }
        InteractionMode::WheelOpen => {
            if cancel {
                wheel.close();
                unsafe { INTERACTION = InteractionMode::Idle; }
            } else if nav_up {
                wheel.select_prev();
            } else if nav_down {
                wheel.select_next();
            } else if confirm || primary {
                // Both Confirm and Primary commit — Primary "does the
                // thing at this cell" in every mode, and inside the
                // wheel that thing is the highlighted option.
                match wheel.current_choice() {
                    action_wheel::WheelChoice::WaterDrop => {
                        if let Some(r) = roster { r.dispatch_water_drop(wheel.anchor); }
                        wheel.close();
                        unsafe { INTERACTION = InteractionMode::Idle; }
                    }
                    action_wheel::WheelChoice::Tanker => {
                        if let Some(r) = roster { r.dispatch_water_tanker(wheel.anchor); }
                        wheel.close();
                        unsafe { INTERACTION = InteractionMode::Idle; }
                    }
                    action_wheel::WheelChoice::Retardant => {
                        // Enter aim mode — the wheel's anchor is the
                        // strip's start point. The player rotates
                        // the cursor to set direction, then commits.
                        aim.begin(wheel.anchor);
                        wheel.close();
                        unsafe { INTERACTION = InteractionMode::RetardantAiming; }
                    }
                    action_wheel::WheelChoice::FireLine => {
                        // First fire-line point is the wheel's anchor.
                        lm.push_point(wheel.anchor);
                        wheel.close();
                        unsafe { INTERACTION = InteractionMode::LineDrafting(Drafter::Truck); }
                    }
                    action_wheel::WheelChoice::HotShot => {
                        // First hot-shot waypoint is the wheel's anchor.
                        lm.push_point(wheel.anchor);
                        wheel.close();
                        unsafe { INTERACTION = InteractionMode::LineDrafting(Drafter::HotShot); }
                    }
                    action_wheel::WheelChoice::Engine => {
                        // Single-cell deploy — engines park at the
                        // anchor and run their hose. Roster rejects
                        // off-road targets, in which case the click
                        // is silently dropped.
                        if let Some(r) = roster {
                            r.dispatch_engine_park(wheel.anchor);
                        }
                        wheel.close();
                        unsafe { INTERACTION = InteractionMode::Idle; }
                    }
                }
            }
        }
        InteractionMode::LineDrafting(drafter) => {
            if cancel {
                lm.discard();
                unsafe { INTERACTION = InteractionMode::Idle; }
            } else if confirm {
                if lm.count > 0 {
                    let mut buf = [UVec3::ZERO; line_mode::LINE_CAP];
                    let n = lm.copy_points_into(&mut buf);
                    if let Some(r) = roster {
                        match drafter {
                            Drafter::Truck   => { r.dispatch_fire_line(&buf[..n]); }
                            Drafter::HotShot => { r.dispatch_hotshot_line(&buf[..n]); }
                        }
                    }
                    // Commit (not discard) — keep the preview voxels
                    // painted as the queued line's in-world marker.
                    lm.commit();
                }
                unsafe { INTERACTION = InteractionMode::Idle; }
            } else if primary {
                lm.push_point(cursor_cell);
            }
        }
        InteractionMode::RetardantAiming => {
            if cancel {
                aim.discard();
                unsafe { INTERACTION = InteractionMode::Idle; }
            } else if confirm || primary {
                if let Some((anchor, dir)) = aim.commit() {
                    // Hand the aimed line to a tanker; the plane
                    // overwrites the magenta preview voxels with
                    // real retardant cell by cell as it flies.
                    let dispatched = roster
                        .as_mut()
                        .map(|r| r.dispatch_retardant_strip(anchor, dir))
                        .unwrap_or(false);
                    if !dispatched {
                        // Pool full — fall back to instant paint so
                        // the player's order isn't silently dropped.
                        retardant_aim::paint_strip(anchor, dir);
                    }
                }
                unsafe { INTERACTION = InteractionMode::Idle; }
            }
        }
    }
}

// ── Per-frame update ─────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    let dt = (dt_ms as f32) / 1000.0;
    // Frame counter drives the surviving_mask cache TTL. Wraps every
    // ~2 years at 60 fps — fine.
    unsafe { FRAME_COUNTER = FRAME_COUNTER.wrapping_add(1); }

    // Read input axes once per frame — sandbox edge events
    // (`_pressed`) only return true the frame the press landed.
    let (mx, my) = input_action_axis2d(unsafe { PAN_ACTION });
    let (ax, ay) = input_action_axis2d(unsafe { AIM_ACTION });
    let zoom = input_action_axis1d(unsafe { ZOOM_ACTION });

    let cam = unsafe { &mut *(&raw mut CAMERA) };
    let cur = unsafe { &mut *(&raw mut CURSOR) };

    // While the wheel is open, WASD navigates the option list — feed
    // a zeroed pan axis to the camera so the world stays put while
    // the player is picking.
    let wheel_open = unsafe { INTERACTION == InteractionMode::WheelOpen };
    let (cam_mx, cam_my) = if wheel_open { (0.0, 0.0) } else { (mx, my) };

    let cam_fx_prev = cam.focus_x;
    let cam_fz_prev = cam.focus_z;
    cam.update(cam_mx, cam_my, zoom, dt);
    // Drag the cursor by the camera's *actual* focus delta (post-clamp)
    // so WASD-pan keeps the reticle on-screen and a clamped camera
    // doesn't push the cursor past the world edge.
    cur.follow_camera(cam.focus_x - cam_fx_prev, cam.focus_z - cam_fz_prev);
    cur.pan(ax, ay, cam.cursor_speed());

    // W/S edge detection on the pan axis — input is Axis2D so we
    // synthesise per-key edges by comparing with last frame. Only
    // meaningful when the wheel is open; ignored elsewhere.
    let (_mx_prev, my_prev) = unsafe { PAN_PREV };
    let nav_up   = my >=  NAV_THRESHOLD && my_prev <  NAV_THRESHOLD;
    let nav_down = my <= -NAV_THRESHOLD && my_prev > -NAV_THRESHOLD;
    unsafe { PAN_PREV = (mx, my); }

    // Input edges. Primary clicks the world, Confirm commits, Cancel
    // backs out. Each button has the same meaning in every mode —
    // see handle_interaction's doc comment for the table. In
    // BALANCE_MODE the cart runs as a passive observer so we force
    // every button to false regardless of physical input — the CSV
    // log captures the no-op-player baseline.
    let (primary_pressed, confirm_pressed, cancel_pressed) = if balance_mode() {
        (false, false, false)
    } else {
        (
            input_action_pressed(unsafe { PRIMARY_ACTION }),
            input_action_pressed(unsafe { CONFIRM_ACTION }),
            input_action_pressed(unsafe { CANCEL_ACTION }),
        )
    };

    // Season state machine — only tick gameplay while the day is
    // active. After SeasonWon / SeasonLost the world freezes (HUD
    // still repaints with the final state).
    let season_active = unsafe {
        (&*(&raw const SEASON))
            .as_ref()
            .map(|s| s.state == season::SeasonState::DayActive)
            .unwrap_or(false)
    };
    if season_active {
        let cursor_cell = {
            let (cx, cz) = cur.cell();
            UVec3::new(cx, cur.marker_y(), cz)
        };
        unsafe {
            handle_interaction(
                cursor_cell,
                primary_pressed, confirm_pressed, cancel_pressed,
                nav_up, nav_down,
            );
        }
        // RetardantAiming reads the cursor every frame, not just on
        // key-edges, so the preview line tracks live as the cursor
        // sweeps around the anchor.
        if unsafe { INTERACTION == InteractionMode::RetardantAiming } {
            unsafe { (&mut *(&raw mut RETARDANT_AIM)).aim_at(cursor_cell); }
        }

        unsafe {
            let fire = &mut *(&raw mut FIRE_STATE);
            fire.tick();
            FIRE_SITES_LAST = fire.burn_site_count();
            if let Some(roster) = &mut *(&raw mut ROSTER) {
                roster.tick();
                let markers = &mut *(&raw mut QUEUE_MARKERS);
                markers.update(roster);
            }
        }

        // Season tick: advance day clock, roll lightning. We define
        // "incident active" loosely — if there's any tracked fire OR
        // any pending order OR any unit doing work, suppress the next
        // strike. Once the player has contained the fire and units
        // are home, the next strike can fire.
        unsafe {
            let incident_active = {
                let fire = &*(&raw const FIRE_STATE);
                let roster = (&*(&raw const ROSTER)).as_ref();
                fire.burn_site_count() > 0
                    || roster.map(|r| {
                        r.queue.pending_total() > 0
                            || r.heli_busy() > 0
                            || r.crew_busy() > 0
                            || r.hotshot_busy() > 0
                            || r.engine_busy() > 0
                    }).unwrap_or(false)
            };

            let strike = (&mut *(&raw mut SEASON))
                .as_mut()
                .and_then(|s| s.tick(dt_ms, incident_active));

            if let Some((sx, sz)) = strike {
                if let Some(p) = terrain::strike_at(sx, sz) {
                    let fire = &mut *(&raw mut FIRE_STATE);
                    fire.add_burn_site(p);
                }
            }

            // Wind sync: SEASON owns the per-day weather. We push the
            // current angle/strength to FireState every frame in case
            // a day rolled over.
            if let Some(s) = &*(&raw const SEASON) {
                let fire = &mut *(&raw mut FIRE_STATE);
                // Only update if the values actually changed — set_wind
                // resets the drift tick so we don't want to do it every
                // frame on the same value.
                if (fire.wind_angle_rad() - s.weather.angle_rad).abs() > 1e-3
                    || (fire.wind_strength() - s.weather.strength).abs() > 1e-3
                {
                    fire.set_wind(s.weather.angle_rad, s.weather.strength);
                }
            }
        }

        if balance_mode() {
            unsafe {
                let alive = surviving_mask();
                let m = build_balance_metrics(alive);
                (&mut *(&raw mut BAL)).tick(&m);
            }
        }

        check_end_conditions();
    }

    // Cursor render is last so it sits on top of any fresh
    // ember / water / firebreak voxels painted this frame.
    cur.render();

    // Sample the host-side binding labels (SPEC §6.5) once per frame
    // so the wheel + HUD both see the same snapshot and repaint
    // automatically on a device switch or rebind.
    let mut buf_pan = [0u8; 16];
    let mut buf_pri = [0u8; 16];
    let mut buf_cnf = [0u8; 16];
    let mut buf_cxl = [0u8; 16];
    let pan_label     = input_action_label(unsafe { PAN_ACTION },     &mut buf_pan);
    let primary_label = input_action_label(unsafe { PRIMARY_ACTION }, &mut buf_pri);
    let confirm_label = input_action_label(unsafe { CONFIRM_ACTION }, &mut buf_cnf);
    let cancel_label  = input_action_label(unsafe { CANCEL_ACTION },  &mut buf_cxl);

    // Wheel render no-ops when the wheel is closed; otherwise
    // repaints when the selection or the confirm-label changes.
    unsafe { (&mut *(&raw mut ACTION_WHEEL)).render(confirm_label); }

    // HUD paints regardless of phase so the player can see the
    // final timer + dot state on the end screen.
    let alive_mask = surviving_mask();
    let ctx = unsafe {
        build_hud_ctx(alive_mask, pan_label, primary_label, confirm_label, cancel_label)
    };
    unsafe { (&mut *(&raw mut HUD)).paint(ctx); }
}

unsafe fn build_hud_ctx<'a>(
    alive_mask: u32,
    pan_label: &'a str,
    primary_label: &'a str,
    confirm_label: &'a str,
    cancel_label: &'a str,
) -> hud::HudCtx<'a> {
    let roster_ref = unsafe { &*(&raw const ROSTER) };
    let fire_ref   = unsafe { &*(&raw const FIRE_STATE) };
    let line_ref   = unsafe { &*(&raw const LINE_MODE) };
    let s          = scenario::get();

    let wind_dir = fire_ref.wind_direction_label();
    let wind_strength = fire_ref.wind_strength_digit();

    let (heli_busy, heli_total, crew_busy, crew_total, hotshot_busy, hotshot_total, engine_busy, engine_total, queue_total) =
        match roster_ref {
            Some(r) => (
                r.heli_busy(), r.heli_total(),
                r.crew_busy(), r.crew_total(),
                r.hotshot_busy(), r.hotshot_total(),
                r.engine_busy(), r.engine_total(),
                r.queue.pending_total(),
            ),
            None => (0, 0, 0, 0, 0, 0, 0, 0, 0),
        };

    // HUD's "TM" countdown becomes the day timer. Day counter +
    // weather glyph + season-end status are routed through
    // additional fields below.
    let (time_left_ms, day_num, day_total, weather_glyph, season_ended, season_won) = unsafe {
        match (&*(&raw const SEASON)).as_ref() {
            Some(s) => (
                season::DAY_DURATION_MS.saturating_sub(s.day_time_ms),
                s.day + 1,
                s.days_total,
                s.weather_glyph(),
                s.state != season::SeasonState::DayActive,
                s.state == season::SeasonState::SeasonWon,
            ),
            None => (0, 1, season::DAYS_PER_SEASON, "----", false, false),
        }
    };

    hud::HudCtx {
        time_left_ms,
        day_num,
        day_total,
        weather_glyph,
        season_ended,
        season_won,
        alive_mask,
        fire_sites:   unsafe { FIRE_SITES_LAST },
        wind_dir,
        wind_strength,
        heli_busy,
        heli_total,
        crew_busy,
        crew_total,
        hotshot_busy,
        hotshot_total,
        engine_busy,
        engine_total,
        tier: s.tier as u32,
        line_mode_active: line_ref.is_drafting(),
        line_mode_count:  line_ref.count as u32,
        queue_total,
        pan_label,
        primary_label,
        confirm_label,
        cancel_label,
    }
}

// ── Render ───────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    unsafe { (&*(&raw const CAMERA)).apply(); }
}

// ── Win / lose ───────────────────────────────────────────────────

/// Cached `surviving_mask` result + the frame at which it was
/// recomputed. Re-scanning every frame is 504 host-import crossings
/// (6 cabins × 7×6×7 voxels × twice per frame for HUD + end check) —
/// at ~80 µs per crossing that's ~40 ms/frame entirely on this one
/// operation, dominating mission-sweep wall time. A 30-frame TTL
/// (~0.5 s of mission) is well below player-perceptible HUD-update
/// latency and below the 5 s BALANCE_MODE log cadence, so the cache
/// is invisible in those contexts. End-of-mission detection is
/// delayed by ≤ 30 frames worst case (≈ 0.5 s).
static mut SURVIVING_MASK_CACHE: u32 = 63; // 6 alive cabins at boot
static mut SURVIVING_MASK_LAST_FRAME: u32 = u32::MAX; // "never computed"
static mut FRAME_COUNTER: u32 = 0;
const SURVIVING_MASK_TTL_FRAMES: u32 = 30;

fn surviving_mask() -> u32 {
    unsafe {
        let frame = FRAME_COUNTER;
        let last = SURVIVING_MASK_LAST_FRAME;
        // u32::MAX sentinel means "never computed yet" — always do a
        // first scan. Subsequent scans honor the TTL.
        if last != u32::MAX && frame.saturating_sub(last) < SURVIVING_MASK_TTL_FRAMES {
            return SURVIVING_MASK_CACHE;
        }
        let mask = recompute_surviving_mask();
        SURVIVING_MASK_CACHE = mask;
        SURVIVING_MASK_LAST_FRAME = frame;
        mask
    }
}

fn recompute_surviving_mask() -> u32 {
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
    let mask = surviving_mask();
    let alive = mask.count_ones();
    // End-of-season win is decided inside Season::advance_day when the
    // 7th day rolls over. Here we only need to catch the cabins-zero
    // early-lose path, which can fire on any day.
    if alive == 0 {
        let still_active = unsafe {
            (&*(&raw const SEASON))
                .as_ref()
                .map(|s| s.state == season::SeasonState::DayActive)
                .unwrap_or(false)
        };
        if still_active {
            unsafe {
                if let Some(s) = (&mut *(&raw mut SEASON)).as_mut() {
                    s.end_lost();
                }
                if balance_mode() {
                    let m = build_balance_metrics(mask);
                    (&mut *(&raw mut BAL)).emit_end("LOST", &m);
                }
            }
        }
        return;
    }
    // Season-end win is fired by Season::advance_day; mirror the
    // balance log here so the harness still captures the outcome.
    let just_won = unsafe {
        (&*(&raw const SEASON))
            .as_ref()
            .map(|s| s.state == season::SeasonState::SeasonWon)
            .unwrap_or(false)
    };
    if just_won && balance_mode() {
        unsafe {
            let m = build_balance_metrics(mask);
            (&mut *(&raw mut BAL)).emit_end("WON", &m);
        }
    }
}

/// Collect a balance-metrics snapshot. Cheap reads from the existing
/// roster/fire/scenario singletons.
unsafe fn build_balance_metrics(alive_mask: u32) -> balance::BalanceMetrics {
    let s = scenario::get();
    let fire_ref = unsafe { &*(&raw const FIRE_STATE) };
    let roster_ref = unsafe { &*(&raw const ROSTER) };

    let wind_label = fire_ref.wind_direction_label();
    let mut wind_dir = [b' '; 2];
    let bytes = wind_label.as_bytes();
    for i in 0..2.min(bytes.len()) { wind_dir[i] = bytes[i]; }

    let (h_b, h_t, c_b, c_t, hs_b, hs_t, e_b, e_t, q) = match roster_ref {
        Some(r) => (
            r.heli_busy(), r.heli_total(),
            r.crew_busy(), r.crew_total(),
            r.hotshot_busy(), r.hotshot_total(),
            r.engine_busy(), r.engine_total(),
            r.queue.pending_total(),
        ),
        None => (0, 0, 0, 0, 0, 0, 0, 0, 0),
    };

    // Total elapsed = (completed days × DAY_DURATION_MS) + day_time_ms.
    // Lets the CSV row still expose a monotonic mission clock across
    // all days of the season.
    let (elapsed_ms, day_num, strikes_today, strikes_total) = unsafe {
        match (&*(&raw const SEASON)).as_ref() {
            Some(s) => (
                (s.day as u32) * season::DAY_DURATION_MS + s.day_time_ms,
                s.day + 1,           // 1..7 for the CSV (human-readable)
                s.strikes_today,
                s.strikes_total,
            ),
            None => (0, 1, 0, 0),
        }
    };

    balance::BalanceMetrics {
        elapsed_ms,
        tier:        s.tier as u32,
        seed:        s.seed,
        fire_sites:  unsafe { FIRE_SITES_LAST },
        alive_count: alive_mask.count_ones(),
        alive_mask,
        wind_dir,
        wind_str:    fire_ref.wind_strength_digit(),
        heli_busy:   h_b,
        heli_total:  h_t,
        crew_busy:   c_b,
        crew_total:  c_t,
        hs_busy:     hs_b,
        hs_total:    hs_t,
        eng_busy:    e_b,
        eng_total:   e_t,
        queue_pending: q,
        day:           day_num,
        strikes_today,
        strikes_total,
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    log("ic cart panicked");
    loop {}
}
