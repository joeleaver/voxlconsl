//! Four ghosts with classic-pacman personalities, body wobble, frightened mode, and chase AI.
//!
//! At every cell boundary each ghost picks the open direction (excluding
//! the 180°-reverse) that minimises manhattan distance to its
//! personality target — Blinky chases the player directly; Pinky aims
//! 4 cells ahead; Inky mirrors Blinky through a point 2 cells ahead of
//! the player; Clyde chases when far and scatters to a corner when
//! close. Power pellets flip the whole crew into frightened mode for
//! `FRIGHTENED_MS` ms — speed drops, direction becomes a random walk,
//! and the body colour swaps to blue with a white-flash tell near the
//! end of the window.
//!
//! Visuals (body wobble, frightened/flash colour swap, eye direction)
//! are computed every frame but repainted only when something changed
//! — a per-ghost cache keeps the host-side traffic flat on still
//! frames.

use voxlconsl_sdk::*;

use crate::{
    M_GHOST_BLINKY, M_GHOST_CLYDE, M_GHOST_EYE, M_GHOST_FLASH,
    M_GHOST_FRIGHTENED, M_GHOST_INKY, M_GHOST_PINKY,
};
use crate::maze::{CELL, Dir, ORIGIN_X, ORIGIN_Z, ROWS, cell_open, dir_open};

// ── Tuning ────────────────────────────────────────────────────────

pub(crate) const GHOST_COUNT: usize = 4;
pub(crate) const GHOST_W: u32 = 5;
const GHOST_SPEED_CPS: f32 = 4.5;
const GHOST_FRIGHTENED_SPEED_CPS: f32 = 3.0;
const FRIGHTENED_MS: u32 = 7_000;
/// Distance threshold for Clyde's scatter behaviour — when farther
/// than this from the player he chases; closer than this he scatters.
const SCATTER_DISTANCE: u32 = 8;

/// Per-ghost squash/stretch body wobble — each ghost flips between
/// two slightly different silhouettes every `WOBBLE_FRAME_MS` so the
/// crew reads as "alive" instead of "static blocks".
const WOBBLE_FRAME_MS: u32 = 150;

/// When `FRIGHTENED_MS_LEFT` drops below this we start the classic-
/// pacman white-flash tell. `FLASH_PERIOD_MS` is the half-period
/// (i.e. duration of one solid colour).
const FLASH_WINDOW_MS: u32 = 2_000;
const FLASH_PERIOD_MS: u32 = 200;

/// "Never painted" sentinel for the per-ghost paint cache, forcing
/// the first repaint after spawn/reset.
const PAINT_PHASE_NEVER: u8 = 255;

// ── Types ─────────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum GhostKind { Blinky, Pinky, Inky, Clyde }

#[derive(Copy, Clone)]
pub(crate) struct Ghost {
    pub(crate) actor: Option<ActorId>,
    pub(crate) kind:  GhostKind,
    pub(crate) home:  (u32, u32),
    pub(crate) cell:  (u32, u32),
    pub(crate) dir:   Dir,
    pub(crate) progress: f32,
    /// Per-ghost frightened flag. Mirrors `FRIGHTENED_MS_LEFT > 0`
    /// except that an *eaten* ghost clears its own flag while the
    /// timer keeps running for the rest of the crew — that's how
    /// an eaten ghost gets its personality colour back while the
    /// others stay blue.
    pub(crate) frightened: bool,
    /// Body-wobble timer + phase.
    wobble_ms: u32,
    wobble_phase: u8,
    /// Cached visual state from the last `repaint_ghost` call.
    paint_dir: Dir,
    paint_mat: u8,
    paint_phase: u8,
}

const fn ghost_init(
    kind: GhostKind, home: (u32, u32), start_dir: Dir, wobble_offset_ms: u32,
) -> Ghost {
    Ghost {
        actor: None,
        kind,
        home,
        cell: home,
        dir: start_dir,
        progress: 0.0,
        frightened: false,
        wobble_ms: wobble_offset_ms,
        wobble_phase: 0,
        paint_dir: Dir::None,
        paint_mat: 0,
        paint_phase: PAINT_PHASE_NEVER,
    }
}

// ── State ─────────────────────────────────────────────────────────
//
// Spawn cells line up with the four `G` markers in MAZE row 13 (the
// ghost-house interior — cols 11, 12, 14, 15). Ghosts navigate north
// toward the maze proper from there. Wobble seeds are staggered so
// the four ghosts don't pulse in perfect lockstep — gives the crew a
// bit of organic life.
pub(crate) static mut GHOSTS: [Ghost; GHOST_COUNT] = [
    ghost_init(GhostKind::Blinky, (11, 13), Dir::North,   0),
    ghost_init(GhostKind::Pinky,  (12, 13), Dir::North,  40),
    ghost_init(GhostKind::Inky,   (14, 13), Dir::North,  80),
    ghost_init(GhostKind::Clyde,  (15, 13), Dir::North, 120),
];

pub(crate) static mut FRIGHTENED_MS_LEFT: u32 = 0;

// ── Boot ──────────────────────────────────────────────────────────

/// Spawn one actor per ghost and paint the initial body so frame-0
/// renders aren't an unpainted blob.
pub(crate) fn init() {
    unsafe {
        for i in 0..GHOST_COUNT {
            let id = actor_spawn().expect("failed to spawn ghost");
            GHOSTS[i].actor = Some(id);
            // Eyes face the spawn direction so they don't look
            // sideways during the very first frame.
            GHOSTS[i].paint_dir = GHOSTS[i].dir;
            repaint_ghost(
                id, ghost_color(GHOSTS[i].kind), GHOSTS[i].dir, GHOSTS[i].wobble_phase,
            );
            GHOSTS[i].paint_mat = ghost_color(GHOSTS[i].kind);
            GHOSTS[i].paint_phase = GHOSTS[i].wobble_phase;
            actor_set_position(id, world_pos(GHOSTS[i].cell, Dir::None, 0.0));
        }
    }
}

// ── Frightened timer ──────────────────────────────────────────────

/// Power pellet → blue ghosts for `FRIGHTENED_MS` ms. We just flip
/// the per-ghost flag here; the per-frame `tick_visuals` notices the
/// cached `paint_mat` no longer matches the intended material and
/// repaints.
pub(crate) fn trigger_frightened() {
    unsafe {
        FRIGHTENED_MS_LEFT = FRIGHTENED_MS;
        for i in 0..GHOST_COUNT {
            GHOSTS[i].frightened = true;
        }
    }
}

pub(crate) fn update_frightened_timer(dt_ms: u32) {
    unsafe {
        if FRIGHTENED_MS_LEFT > 0 {
            let prev = FRIGHTENED_MS_LEFT;
            FRIGHTENED_MS_LEFT = FRIGHTENED_MS_LEFT.saturating_sub(dt_ms);
            if prev > 0 && FRIGHTENED_MS_LEFT == 0 {
                // Falling edge — drop all ghosts out of frightened
                // mode. The visual tick handles the repaint.
                for i in 0..GHOST_COUNT {
                    GHOSTS[i].frightened = false;
                }
            }
        }
    }
}

// ── Movement ──────────────────────────────────────────────────────

/// Step all four ghosts forward by `dt` seconds. Crossing a cell
/// boundary triggers a fresh direction pick via `pick_dir`.
pub(crate) fn update(dt: f32, player_cell: (u32, u32), player_dir: Dir) {
    let frightened = unsafe { FRIGHTENED_MS_LEFT > 0 };
    let speed = if frightened { GHOST_FRIGHTENED_SPEED_CPS } else { GHOST_SPEED_CPS };

    // Snapshot Blinky's cell up-front so Inky's target can reference
    // it without aliasing borrows on `GHOSTS`.
    let blinky_cell = unsafe { GHOSTS[0].cell };

    for i in 0..GHOST_COUNT {
        let mut g = unsafe { GHOSTS[i] };
        g.progress += speed * dt;
        while g.progress >= 1.0 {
            let (dc, dr) = g.dir.delta();
            let next = (
                (g.cell.0 as i32 + dc) as u32,
                (g.cell.1 as i32 + dr) as u32,
            );
            // `g.dir` should always be an open direction (we picked it
            // last time we crossed a cell) — but defensively stop if
            // it isn't.
            if !cell_open(next.0 as i32, next.1 as i32) {
                g.progress = 0.0;
                g.dir = Dir::None;
                break;
            }
            g.cell = next;
            g.progress -= 1.0;
            g.dir = pick_dir(&g, player_cell, player_dir, blinky_cell, frightened);
            if matches!(g.dir, Dir::None) {
                // Boxed in — stay put until something opens up. Rare.
                g.progress = 0.0;
                break;
            }
        }
        unsafe {
            GHOSTS[i] = g;
            if let Some(actor) = g.actor {
                actor_set_position(actor, world_pos(g.cell, g.dir, g.progress));
            }
        }
    }
}

/// Pick a ghost's next direction at a cell boundary. Excludes the
/// 180°-reverse (classic pacman behaviour) and walls. Frightened
/// ghosts wander randomly; otherwise we head toward the ghost's
/// personality target and minimise manhattan distance.
fn pick_dir(
    g: &Ghost,
    player_cell: (u32, u32),
    player_dir: Dir,
    blinky_cell: (u32, u32),
    frightened: bool,
) -> Dir {
    let reverse = g.dir.opposite();
    let mut candidates: [(Dir, u32); 4] = [(Dir::None, 0); 4];
    let mut n = 0;
    for &d in &[Dir::North, Dir::East, Dir::South, Dir::West] {
        if d == reverse { continue; }
        if !dir_open(g.cell, d) { continue; }
        let (dc, dr) = d.delta();
        let nc = (g.cell.0 as i32 + dc) as u32;
        let nr = (g.cell.1 as i32 + dr) as u32;
        candidates[n] = (d, encode_step(nc, nr));
        n += 1;
    }
    if n == 0 {
        // Allow reverse as a fallback (dead end).
        if dir_open(g.cell, reverse) { return reverse; }
        return Dir::None;
    }
    if frightened {
        let pick = (crate::rng::u32_() as usize) % n;
        return candidates[pick].0;
    }
    let target = target_cell(g, player_cell, player_dir, blinky_cell);
    let mut best = candidates[0].0;
    let mut best_d = manhattan_from_step(candidates[0].1, target);
    for &(d, step) in &candidates[1..n] {
        let m = manhattan_from_step(step, target);
        if m < best_d { best_d = m; best = d; }
    }
    best
}

fn target_cell(
    g: &Ghost, player_cell: (u32, u32), player_dir: Dir, blinky_cell: (u32, u32),
) -> (i32, i32) {
    match g.kind {
        GhostKind::Blinky => (player_cell.0 as i32, player_cell.1 as i32),
        GhostKind::Pinky => {
            // 4 cells ahead of the player.
            let (dc, dr) = player_dir.delta();
            (player_cell.0 as i32 + dc * 4, player_cell.1 as i32 + dr * 4)
        }
        GhostKind::Inky => {
            // Pivot point = player + 2 ahead; target = mirror of
            // Blinky through that pivot (classic Inky behaviour).
            let (dc, dr) = player_dir.delta();
            let pc = player_cell.0 as i32 + dc * 2;
            let pr = player_cell.1 as i32 + dr * 2;
            (2 * pc - blinky_cell.0 as i32, 2 * pr - blinky_cell.1 as i32)
        }
        GhostKind::Clyde => {
            let d = manhattan_cells(g.cell, player_cell);
            if d > SCATTER_DISTANCE {
                (player_cell.0 as i32, player_cell.1 as i32)
            } else {
                // Scatter corner — bottom-left.
                (1, ROWS as i32 - 2)
            }
        }
    }
}

fn encode_step(col: u32, row: u32) -> u32 { (col << 16) | row }

fn manhattan_from_step(step: u32, target: (i32, i32)) -> u32 {
    let col = (step >> 16) as i32;
    let row = (step & 0xFFFF) as i32;
    ((col - target.0).abs() + (row - target.1).abs()) as u32
}

fn manhattan_cells(a: (u32, u32), b: (u32, u32)) -> u32 {
    let dx = (a.0 as i32 - b.0 as i32).abs() as u32;
    let dy = (a.1 as i32 - b.1 as i32).abs() as u32;
    dx + dy
}

// ── Visuals ───────────────────────────────────────────────────────

/// Per-frame visual update for all four ghosts: advance the body-
/// wobble phase, pick the right colour (personality / frightened /
/// flash), and repaint only when something visually changed since the
/// last frame.
pub(crate) fn tick_visuals(dt_ms: u32) {
    let timer = unsafe { FRIGHTENED_MS_LEFT };
    // Globally-timed flash phase so all blue ghosts flash in sync —
    // matches the classic-pacman tell.
    let in_flash_window = timer > 0 && timer < FLASH_WINDOW_MS;
    let flash_white = in_flash_window && (timer / FLASH_PERIOD_MS) & 1 == 0;

    for i in 0..GHOST_COUNT {
        let mut g = unsafe { GHOSTS[i] };
        let actor = match g.actor { Some(a) => a, None => continue };

        g.wobble_ms = g.wobble_ms.saturating_add(dt_ms);
        while g.wobble_ms >= WOBBLE_FRAME_MS {
            g.wobble_ms -= WOBBLE_FRAME_MS;
            g.wobble_phase ^= 1;
        }

        let body_mat = if g.frightened {
            if flash_white { M_GHOST_FLASH } else { M_GHOST_FRIGHTENED }
        } else {
            ghost_color(g.kind)
        };

        // Eyes keep facing the last non-None direction so a briefly-
        // stopped ghost doesn't snap back to a default east stare.
        let face = if matches!(g.dir, Dir::None) { g.paint_dir } else { g.dir };

        if body_mat != g.paint_mat
            || g.wobble_phase != g.paint_phase
            || face != g.paint_dir
            || g.paint_phase == PAINT_PHASE_NEVER
        {
            repaint_ghost(actor, body_mat, face, g.wobble_phase);
            g.paint_mat   = body_mat;
            g.paint_phase = g.wobble_phase;
            g.paint_dir   = face;
        }

        unsafe { GHOSTS[i] = g; }
    }
}

/// Paint a ghost's 5×5×5 actor volume: scallop-bottom body, head dome,
/// eyes pointing in `dir`, and a 1-voxel squash on the alternate
/// `phase` so the body subtly wobbles between frames.
fn repaint_ghost(actor: ActorId, body: u8, dir: Dir, phase: u8) {
    actor_clear(actor);

    // ── Body column ──────────────────────────────────────────────
    // y=0 is the scalloped "skirt" (handled below); the dome sits
    // above. Phase 1 squashes the dome by 1 voxel — reads as a gentle
    // bob from the high camera.
    let body_top: u8 = if phase == 0 { 4 } else { 3 };
    actor_fill_box(
        actor,
        U8Vec3::new(0, 1, 0),
        U8Vec3::new(GHOST_W as u8 - 1, body_top, GHOST_W as u8 - 1),
        body,
    );

    // ── Skirt scallop on y=0 ─────────────────────────────────────
    // Two interleaving patterns — alternate with `phase` so the ghost
    // looks like it's walking on a row of little legs.
    let skirt_xs: [u8; 3] = if phase == 0 { [0, 2, 4] } else { [1, 3, 4] };
    for &x in &skirt_xs {
        actor_set_voxel(actor, U8Vec3::new(x, 0, 0), body);
        actor_set_voxel(actor, U8Vec3::new(x, 0, 2), body);
        actor_set_voxel(actor, U8Vec3::new(x, 0, 4), body);
    }

    paint_eyes(actor, dir, body_top);
}

/// Paint two darker eye voxels onto the face of the ghost body that
/// matches `dir`. `body_top` is the topmost solid y on the dome — we
/// place the eyes one voxel below so they don't disappear during the
/// squash frame.
fn paint_eyes(actor: ActorId, dir: Dir, body_top: u8) {
    let eye_y = body_top.saturating_sub(1).max(2);
    let (a, b) = match dir {
        Dir::East  => (U8Vec3::new(4, eye_y, 1), U8Vec3::new(4, eye_y, 3)),
        Dir::West  => (U8Vec3::new(0, eye_y, 1), U8Vec3::new(0, eye_y, 3)),
        Dir::North => (U8Vec3::new(1, eye_y, 0), U8Vec3::new(3, eye_y, 0)),
        Dir::South => (U8Vec3::new(1, eye_y, 4), U8Vec3::new(3, eye_y, 4)),
        // Stationary in-house: face east by default so eyes still
        // read instead of vanishing into the body.
        Dir::None  => (U8Vec3::new(4, eye_y, 1), U8Vec3::new(4, eye_y, 3)),
    };
    actor_set_voxel(actor, a, M_GHOST_EYE);
    actor_set_voxel(actor, b, M_GHOST_EYE);
}

pub(crate) fn ghost_color(kind: GhostKind) -> u8 {
    match kind {
        GhostKind::Blinky => M_GHOST_BLINKY,
        GhostKind::Pinky  => M_GHOST_PINKY,
        GhostKind::Inky   => M_GHOST_INKY,
        GhostKind::Clyde  => M_GHOST_CLYDE,
    }
}

/// World position of a ghost's local `(0,_,0)` corner — same
/// convention as the player.
pub(crate) fn world_pos(cell: (u32, u32), dir: Dir, progress: f32) -> Vec3 {
    let (col, row) = cell;
    let (dc, dr) = dir.delta();
    let x = (col as f32 + 0.5 + dc as f32 * progress) * CELL as f32 + ORIGIN_X as f32;
    let z = (row as f32 + 0.5 + dr as f32 * progress) * CELL as f32 + ORIGIN_Z as f32;
    Vec3::new(
        x - GHOST_W as f32 * 0.5,
        1.0,
        z - GHOST_W as f32 * 0.5,
    )
}

/// Reset every ghost to its spawn cell and invalidate paint caches so
/// the next visual tick repaints the spawn frame correctly. Used by
/// the death + restart paths.
pub(crate) fn reset_all() {
    unsafe {
        FRIGHTENED_MS_LEFT = 0;
        for i in 0..GHOST_COUNT {
            let home = GHOSTS[i].home;
            let actor = GHOSTS[i].actor;
            GHOSTS[i].cell = home;
            GHOSTS[i].progress = 0.0;
            GHOSTS[i].dir = Dir::None;
            GHOSTS[i].frightened = false;
            // Invalidate paint cache so the next visual tick
            // repaints the eyes for the new (None) facing.
            GHOSTS[i].paint_phase = PAINT_PHASE_NEVER;
            if let Some(a) = actor {
                actor_set_position(a, world_pos(home, Dir::None, 0.0));
            }
        }
    }
}
