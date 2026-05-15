//! voxdude — a voxel pacman clone.
//!
//! Top-down voxel maze. The player roams a 28×31 grid eating dots
//! and dodging four ghosts; grabbing one of the four power pellets
//! briefly flips the ghosts into a frightened state so the player can
//! eat *them* instead. Clear every dot to win, lose three lives to
//! lose. **K** restarts after the round ends.
//!
//! ## What this cart demonstrates
//!
//! - **Multi-actor coordination** — player + 4 ghosts + 24 particles
//!   + 6 score digits + 3 life icons, all repositioned every frame.
//! - **Prefab + CoW + flipbook animation** — the chomper cycles two
//!   prefab frames; lives + score digits all share their prefabs.
//! - **Camera-relative HUD** — score and lives ride dedicated actors
//!   that re-anchor to the player's centre every frame, so they read
//!   as screen-space overlay even though they're world-space voxels.
//! - **MIDI music + per-channel chiptune patches** — bundled SMF with
//!   cart-side `program_change` routing onto five custom synth
//!   patches; SFX one-shots layered on top via `voice_trigger`.
//! - **Material recolouring** — win/lose flashes the whole maze
//!   colour by `material_define`-ing the wall slots in place, no
//!   voxel re-emission.
//!
//! ## File map
//!
//! | File | What lives there |
//! |---|---|
//! | `lib.rs`     | Entry points + game state + win/lose orchestration |
//! | `maze.rs`    | Board data, painting, spatial queries |
//! | `player.rs`  | Chomper prefab + grid-locked movement |
//! | `ghosts.rs`  | Four-ghost AI + frightened/flash visuals |
//! | `particles.rs` | Pooled chomp-burst sparkles |
//! | `hud.rs`     | Camera-relative score + lives |
//! | `audio.rs`   | Music routing + SFX note constants |
//! | `rng.rs`     | Cart-local xorshift32 |

#![no_std]
#![no_main]

use voxlconsl_sdk::*;
use voxlconsl_sdk::audio as sdk_audio;

mod audio;
mod ghosts;
mod hud;
mod maze;
mod particles;
mod player;
mod rng;

use crate::maze::{Dir, DOT_GRID, DOTS_REMAINING};

// ── Material slots (must match materials.toml) ────────────────────

pub(crate) const M_WALL:              u8 = 1;
pub(crate) const M_FLOOR:             u8 = 2;
pub(crate) const M_DOT:               u8 = 3;
pub(crate) const M_POWER_PELLET:      u8 = 4;
pub(crate) const M_PLAYER:            u8 = 5;
pub(crate) const M_GHOST_BLINKY:      u8 = 6;
pub(crate) const M_GHOST_PINKY:       u8 = 7;
pub(crate) const M_GHOST_INKY:        u8 = 8;
pub(crate) const M_GHOST_CLYDE:       u8 = 9;
pub(crate) const M_GHOST_FRIGHTENED:  u8 = 10;
pub(crate) const M_PARTICLE:          u8 = 11;
pub(crate) const M_GHOST_EYE:         u8 = 12;
pub(crate) const M_GHOST_FLASH:       u8 = 13;
pub(crate) const M_WALL_OUTER:        u8 = 14;
pub(crate) const M_WALL_CAP:          u8 = 15;
pub(crate) const M_WALL_PIP:          u8 = 16;
pub(crate) const M_HUD_SCORE:         u8 = 17;
pub(crate) const M_HUD_OUTLINE:       u8 = 18;

// ── Score / game state ────────────────────────────────────────────

const DOT_VALUE:          u32 = 10;
const POWER_PELLET_VALUE: u32 = 50;
const GHOST_VALUE:        u32 = 200;
const STARTING_LIVES:     u32 = 3;

#[derive(Copy, Clone, PartialEq, Eq)]
enum GameState { Playing, Won, Lost }

static mut STATE:  GameState = GameState::Playing;
static mut SCORE:  u32 = 0;
static mut LIVES:  u32 = STARTING_LIVES;
static mut RESTART_ACTION: ActionHandle = ActionHandle(0);

// ── Boot ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    // Space-cabinet sky — a saturated purple void around the maze so
    // the screen reads "you're inside an arcade backdrop", not just
    // floating in black.
    sky_set_gradient(
        Material::pack_color(8, 1), // purple:1 — vivid ceiling
        Material::pack_color(8, 0), // purple:0 — placeholder horizon
                                    //            for when the renderer
                                    //            grows a real gradient.
    );
    // Top-down lighting — sun straight down so wall tops are lit and
    // the floor space stays dark and quiet.
    light_set_sun(Vec3::new(0.0, -1.0, 0.0), 0, 0);

    maze::paint_maze();

    camera_set_fov(60.0);
    set_follow_camera();

    player::init();
    ghosts::init();
    particles::init();
    hud::init();

    // Restart action — K on the browser host. The host's binding-hint
    // table doesn't carry a dedicated "restart" hint, so we reuse
    // `SecondaryFire` which defaults to K. Pressing K after winning
    // or losing starts a fresh round.
    unsafe {
        RESTART_ACTION = input_declare_action(
            ActionKind::Button, BindingHint::SecondaryFire, "restart",
        );
    }

    audio::init_music();
}

// ── Per-frame update ─────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    let dt = (dt_ms as f32) / 1000.0;

    // End-of-game state freezes movement; the restart button (K)
    // brings the cart back to a fresh round. We still tick the HUD
    // so the score/lives stay readable during the freeze.
    if !matches!(unsafe { STATE }, GameState::Playing) {
        if input_action_pressed(unsafe { RESTART_ACTION }) {
            restart_game();
        }
        hud::tick(unsafe { SCORE }, unsafe { LIVES });
        return;
    }

    // ── Read input + buffer the desired direction ────────────────
    let (mx, my) = input_action_axis2d(unsafe { player::MOVE_ACTION });
    let want = player::quantise_axis(mx, my);
    if !matches!(want, Dir::None) {
        unsafe { player::DESIRED_DIR = want; }
    }

    // ── Player movement ──────────────────────────────────────────
    let current_cell = player::tick_movement(dt, dt_ms);

    // ── Dot collection ───────────────────────────────────────────
    let (pc, pr) = current_cell;
    let won = collect_dot_at(pc, pr);
    if won { return; }

    // ── Ghost tick + collisions ──────────────────────────────────
    ghosts::update_frightened_timer(dt_ms);
    ghosts::update(dt, current_cell, unsafe { player::PLAYER_DIR });
    ghosts::tick_visuals(dt_ms);
    check_ghost_collisions();

    // ── Cosmetic systems ─────────────────────────────────────────
    particles::tick(dt_ms);

    // HUD ticks LAST so it anchors to the same final player state the
    // renderer will use in `render` — earlier in the function some
    // player-state fields are still mid-update; reading
    // `player::world_centre` there would give a one-frame-old anchor
    // while the camera reads the new one, producing visible HUD
    // drift in the direction of motion.
    hud::tick(unsafe { SCORE }, unsafe { LIVES });
}

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    set_follow_camera();
}

// ── Dot collection ────────────────────────────────────────────────

/// Collect any dot/power-pellet at `(col, row)`. Updates `SCORE`,
/// emits the chomp-burst sparkle and the appropriate SFX, and returns
/// `true` if the cart just transitioned to the `Won` state — caller
/// should early-return from `update`.
fn collect_dot_at(col: u32, row: u32) -> bool {
    unsafe {
        let kind = DOT_GRID[row as usize][col as usize];
        if kind == 0 { return false; }
        DOT_GRID[row as usize][col as usize] = 0;
        DOTS_REMAINING = DOTS_REMAINING.saturating_sub(1);
        SCORE += match kind {
            2 => POWER_PELLET_VALUE,
            _ => DOT_VALUE,
        };
        maze::clear_dot_cell(col, row, kind);

        // Power pellets get a bigger burst — visually and score-wise
        // they're worth more, the bigger burst reads as more reward.
        let burst = if kind == 2 { particles::PER_BURST + 3 } else { particles::PER_BURST };
        particles::spawn_burst(player::world_centre(), burst);

        if kind == 2 {
            ghosts::trigger_frightened();
            let _ = sdk_audio::voice_trigger(audio::PATCH_PING, audio::NOTE_POWER_PELLET, 110);
        } else {
            let _ = sdk_audio::voice_trigger(audio::PATCH_CHOMP, audio::NOTE_CHOMP, 90);
        }

        if DOTS_REMAINING == 0 {
            let _ = sdk_audio::voice_trigger(audio::PATCH_PING, audio::NOTE_WIN, 120);
            enter_won();
            return true;
        }
    }
    false
}

// ── Ghost collisions ──────────────────────────────────────────────

/// Player ↔ ghost cell overlap. In frightened mode the player eats
/// the ghost (+200, ghost respawns); otherwise the player loses a
/// life and gets reset to spawn.
fn check_ghost_collisions() {
    let pcell = unsafe { player::PLAYER_CELL };
    let frightened = unsafe { ghosts::FRIGHTENED_MS_LEFT > 0 };
    for i in 0..ghosts::GHOST_COUNT {
        let g = unsafe { ghosts::GHOSTS[i] };
        if g.cell != pcell { continue; }
        if frightened {
            unsafe {
                SCORE += GHOST_VALUE;
                ghosts::GHOSTS[i].cell = g.home;
                ghosts::GHOSTS[i].progress = 0.0;
                ghosts::GHOSTS[i].dir = Dir::None;
                // Eaten ghost escapes frightened state immediately —
                // the visual tick will repaint it to its personality
                // colour next frame. Others stay blue until the
                // global timer ends.
                ghosts::GHOSTS[i].frightened = false;
                if let Some(actor) = g.actor {
                    actor_set_position(
                        actor,
                        ghosts::world_pos(g.home, Dir::None, 0.0),
                    );
                }
            }
            let _ = sdk_audio::voice_trigger(
                audio::PATCH_PING, audio::NOTE_GHOST_EATEN, 110,
            );
        } else {
            reset_after_death();
            return;
        }
    }
}

fn reset_after_death() {
    player::reset_to_spawn();
    ghosts::reset_all();
    unsafe { LIVES = LIVES.saturating_sub(1); }
    let _ = sdk_audio::voice_trigger(audio::PATCH_PING, audio::NOTE_DEATH, 120);
    if unsafe { LIVES } == 0 {
        enter_lost();
    }
}

// ── Win / lose / restart ─────────────────────────────────────────

/// Win — re-emit every wall in green and freeze the game loop.
fn enter_won() {
    unsafe { STATE = GameState::Won; }
    recolor_walls(Material::pack_color(3, 3));   // grass_green:3
}

/// Lose — re-emit every wall in red and freeze the game loop.
fn enter_lost() {
    unsafe { STATE = GameState::Lost; }
    recolor_walls(Material::pack_color(10, 3));  // red:3
}

/// Recolour every wall material slot in one shot — used to flash the
/// whole maze green on win / red on lose. We re-define each material
/// slot at the host level rather than re-emitting voxels, so the
/// change is O(slots) regardless of maze size and avoids disturbing
/// the dot/pip footprints already in the world.
fn recolor_walls(color: u8) {
    let flags = MaterialFlags::empty().with(MaterialFlags::GLOSSY);
    material_define(M_WALL,       color, 4,  flags);
    material_define(M_WALL_OUTER, color, 6,  flags);
    material_define(M_WALL_CAP,   color, 10, flags);
    material_define(M_WALL_PIP,   color, 15, MaterialFlags::empty());
}

/// Restore the per-slot wall materials to their materials.toml
/// defaults — called from `restart_game` after a win/lose flash.
fn reset_wall_materials() {
    let flags = MaterialFlags::empty().with(MaterialFlags::GLOSSY);
    material_define(M_WALL,       Material::pack_color(7, 1),  4,  flags);
    material_define(M_WALL_OUTER, Material::pack_color(7, 2),  6,  flags);
    material_define(M_WALL_CAP,   Material::pack_color(6, 2),  10, flags);
    material_define(M_WALL_PIP,   Material::pack_color(13, 3), 15, MaterialFlags::empty());
}

fn restart_game() {
    unsafe {
        STATE = GameState::Playing;
        LIVES = STARTING_LIVES;
        SCORE = 0;
        DOTS_REMAINING = 0;
        // Clear out any leftover dot voxels from the prior round.
        for r in 0..maze::ROWS as usize {
            for c in 0..maze::COLS as usize {
                let k = DOT_GRID[r][c];
                if k != 0 {
                    maze::clear_dot_cell(c as u32, r as u32, k);
                    DOT_GRID[r][c] = 0;
                }
            }
        }
    }
    // Restore wall slots flattened by the win/lose flash.
    reset_wall_materials();
    // Reseed dots + reset ghost spawn positions.
    maze::paint_maze();
    // Force a HUD repaint on the next tick — without this, the
    // skip-if-unchanged path would keep the previous round's number
    // on screen when restarting after a win.
    hud::invalidate_score_cache();
    ghosts::reset_all();
    particles::clear_all();
    player::reset_to_spawn();
}

// ── Camera ────────────────────────────────────────────────────────

/// Position the camera over the player at low altitude with a slight
/// southward tilt so wall sides are visible (depth/3D feel) without
/// the side-asymmetry dominating the frame — that's why the walls
/// are kept short (`maze::WALL_H = 2`).
///
/// Eye sits ~7 cells above the ground; the look target is offset
/// south of the eye so the camera tilts back, putting the player
/// roughly a third of the way up the screen — extra forward
/// visibility when moving north.
fn set_follow_camera() {
    const EYE_HEIGHT: f32 = 56.0;
    const TILT_Z: f32 = 16.0;
    let world = player::world_centre();
    camera_set_lookat(
        Vec3::new(world.x, EYE_HEIGHT, world.z + TILT_Z),
        Vec3::new(world.x, 0.0,        world.z),
        Vec3::new(0.0,     0.0,       -1.0),
    );
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    log("cart panicked");
    loop {}
}
