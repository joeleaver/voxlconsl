//! The pacman-style chomper — prefab, animation, grid-locked movement.
//!
//! Two prefab frames (mouth closed / mouth open) are baked at boot and
//! cycled via the SDK `Flipbook` helper at 80 ms/frame whenever the
//! player is moving. Standing still snaps the mouth back closed.
//!
//! Movement is grid-locked à la classic pacman: the player sits on a
//! cell, moves toward an adjacent cell in `PLAYER_DIR`, and
//! `PLAYER_PROGRESS ∈ [0, 1)` tracks the lerp from cell-centre to
//! cell-centre. Direction changes buffer in `DESIRED_DIR` and apply at
//! the next cell boundary if open. The 180° reverse is allowed mid-cell.

use voxlconsl_sdk::*;
use voxlconsl_sdk::animation::Flipbook;

use crate::M_PLAYER;
use crate::maze::{CELL, Dir, ORIGIN_X, ORIGIN_Z};

// ── Geometry ──────────────────────────────────────────────────────

pub(crate) const PLAYER_W: u32 = 5;
const PLAYER_SPEED_CPS: f32 = 5.5;

const DUDE_W: usize = 5;
const DUDE_H: usize = 3;
const DUDE_D: usize = 5;
const DUDE_VOL: usize = DUDE_W * DUDE_H * DUDE_D;

pub(crate) const P_CHOMP_CLOSED: PrefabId = PrefabId(1);
pub(crate) const P_CHOMP_OPEN:   PrefabId = PrefabId(2);

static mut DENSE_CHOMP_0: [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_CHOMP_1: [u8; DUDE_VOL] = [0; DUDE_VOL];

const CHOMP_FRAMES: &[PrefabId] = &[P_CHOMP_CLOSED, P_CHOMP_OPEN];
static mut CHOMP_FB: Flipbook = Flipbook::new(CHOMP_FRAMES, 80, true);
static mut CURRENT_FRAME: PrefabId = P_CHOMP_CLOSED;

// ── State ─────────────────────────────────────────────────────────

/// Player facing in radians (yaw around +Y). Updated whenever the
/// player has a current direction; preserved while stopped so the
/// dude keeps facing the way he last moved.
static mut PLAYER_FACING: f32 = 0.0;

pub(crate) static mut PLAYER:          Option<ActorId> = None;
pub(crate) static mut PLAYER_CELL:     (u32, u32)      = (13, 23); // matches the `P` in MAZE
pub(crate) static mut PLAYER_DIR:      Dir             = Dir::None;
pub(crate) static mut PLAYER_PROGRESS: f32             = 0.0;
pub(crate) static mut DESIRED_DIR:     Dir             = Dir::None;
pub(crate) static mut MOVE_ACTION:     ActionHandle    = ActionHandle(0);

/// Spawn cell — used for restart after death or new game.
pub(crate) const SPAWN_CELL: (u32, u32) = (13, 23);

// ── Boot ──────────────────────────────────────────────────────────

/// Bake the two chomp prefabs, spawn the player actor at the spawn
/// cell, and register the movement input action.
pub(crate) fn init() {
    unsafe {
        bake_chomp_frame(&raw mut DENSE_CHOMP_0, /*open*/ false);
        bake_chomp_frame(&raw mut DENSE_CHOMP_1, /*open*/ true);
        let size = U8Vec3::new(DUDE_W as u8, DUDE_H as u8, DUDE_D as u8);
        prefab_define(P_CHOMP_CLOSED, &*(&raw const DENSE_CHOMP_0), size);
        prefab_define(P_CHOMP_OPEN,   &*(&raw const DENSE_CHOMP_1), size);
    }
    let id = actor_spawn_from(P_CHOMP_CLOSED, Orientation::Up)
        .expect("failed to spawn player");
    unsafe {
        PLAYER = Some(id);
        CURRENT_FRAME = P_CHOMP_CLOSED;
        actor_set_position(id, world_pos(PLAYER_CELL, Dir::None, 0.0));

        // Reuse the standard PrimaryMovement Axis2D binding (WASD on
        // browser). We quantise to a cardinal direction inside `update`
        // so gamepad sticks work too.
        MOVE_ACTION = input_declare_action(
            ActionKind::Axis2D, BindingHint::PrimaryMovement, "move",
        );
    }
}

/// Bake one chomp frame into `dense`. Voxels live on a 5×3×5 grid
/// (x-fastest, then y, then z — matches `prefab_define`'s layout).
/// When `open == true`, a triangular wedge is cut out of the `+x` face
/// to form the mouth.
unsafe fn bake_chomp_frame(dense: *mut [u8; DUDE_VOL], open: bool) {
    let dense = unsafe { &mut *dense };
    *dense = [0; DUDE_VOL];
    for y in 0..DUDE_H {
        for z in 0..DUDE_D {
            for x in 0..DUDE_W {
                if !in_disk(x, z) { continue; }
                if open && in_mouth_wedge(x, z) { continue; }
                let idx = x + y * DUDE_W + z * DUDE_W * DUDE_H;
                dense[idx] = M_PLAYER;
            }
        }
    }
}

/// `(x, z)` is part of the closed-pacman disk (5×5 with corners
/// chamfered off → 21-voxel roughly-round footprint).
fn in_disk(x: usize, z: usize) -> bool {
    !((x == 0 || x == 4) && (z == 0 || z == 4))
}

/// `(x, z)` is removed when the mouth is open. Wedge points `+x`:
/// widest cut at the middle row (z=2), narrowing toward top/bottom.
fn in_mouth_wedge(x: usize, z: usize) -> bool {
    match z {
        0 | 4 => x == 3,
        1 | 3 => x == 4,
        2     => x == 2 || x == 3 || x == 4,
        _ => false,
    }
}

// ── Per-frame visual update ──────────────────────────────────────

/// Advance the chomp flipbook and rotate the player to face `dir`.
/// Called every frame from the cart's `update`.
pub(crate) unsafe fn advance_chomp(actor: ActorId, dir: Dir, dt_ms: u32) {
    let moving = !matches!(dir, Dir::None);
    let want = if moving {
        let fb = unsafe { &mut *(&raw mut CHOMP_FB) };
        fb.tick(dt_ms);
        fb.current()
    } else {
        // Snap mouth shut when stopped.
        P_CHOMP_CLOSED
    };
    if want != unsafe { CURRENT_FRAME } {
        actor_set_prefab(actor, want);
        unsafe { CURRENT_FRAME = want; }
    }

    if moving {
        // Yaw rotates the prefab around +Y. Prefab is authored facing
        // `+x` (east) so east → 0 rad. Going clockwise (+yaw) turns
        // toward south (+z) under voxlconsl's right-handed convention.
        let yaw = match dir {
            Dir::East  => 0.0,
            Dir::South => core::f32::consts::FRAC_PI_2,
            Dir::West  => core::f32::consts::PI,
            Dir::North => -core::f32::consts::FRAC_PI_2,
            Dir::None  => unsafe { PLAYER_FACING },
        };
        unsafe { PLAYER_FACING = yaw; }
        actor_set_yaw(actor, yaw);
    }
}

// ── Movement / positioning ───────────────────────────────────────

/// Advance one frame of grid-locked movement. Updates `PLAYER_CELL`,
/// `PLAYER_DIR`, `PLAYER_PROGRESS` and pushes the new world position
/// into the actor. Returns the new cell so callers (e.g. dot
/// collection) can act on it.
pub(crate) fn tick_movement(dt: f32, dt_ms: u32) -> (u32, u32) {
    let cell = unsafe { PLAYER_CELL };
    let mut dir = unsafe { PLAYER_DIR };
    let desired = unsafe { DESIRED_DIR };
    let mut progress = unsafe { PLAYER_PROGRESS };

    // 180° reverse anywhere — invert direction and the progress (the
    // player keeps the same sub-cell offset, just going the other way).
    if !matches!(desired, Dir::None) && desired == dir.opposite() {
        dir = desired;
        progress = 1.0 - progress;
    }

    // Stopped + can start moving in the desired direction.
    if matches!(dir, Dir::None) && crate::maze::dir_open(cell, desired) {
        dir = desired;
        progress = 0.0;
    }

    // Advance progress; cross as many cells as needed in a single
    // frame (pathological but cheap to handle).
    let step = PLAYER_SPEED_CPS * dt;
    progress += step;
    let mut current_cell = cell;
    while progress >= 1.0 && !matches!(dir, Dir::None) {
        let (dc, dr) = dir.delta();
        let next_col = current_cell.0 as i32 + dc;
        let next_row = current_cell.1 as i32 + dr;
        // If the next cell is a wall, we never should have started
        // toward it — defensively stop and clamp.
        if !crate::maze::cell_open(next_col, next_row) {
            progress = 0.0;
            dir = Dir::None;
            break;
        }
        current_cell = (next_col as u32, next_row as u32);
        progress -= 1.0;

        // At the new cell, evaluate turning: prefer the buffered
        // desired direction, else continue, else stop.
        if !matches!(desired, Dir::None)
            && desired != dir
            && crate::maze::dir_open(current_cell, desired)
        {
            dir = desired;
        } else if !crate::maze::dir_open(current_cell, dir) {
            dir = Dir::None;
            progress = 0.0;
            break;
        }
    }

    unsafe {
        PLAYER_CELL = current_cell;
        PLAYER_DIR = dir;
        PLAYER_PROGRESS = progress;
        if let Some(actor) = PLAYER {
            actor_set_position(actor, world_pos(current_cell, dir, progress));
            advance_chomp(actor, dir, dt_ms);
        }
    }
    current_cell
}

/// Map an Axis2D reading to a cardinal direction. Dominant-axis wins
/// (so a slightly off-axis WASD press still resolves cleanly), with a
/// dead zone so a sticky gamepad doesn't spam direction changes.
pub(crate) fn quantise_axis(mx: f32, my: f32) -> Dir {
    const DEAD: f32 = 0.4;
    if mx.abs() > my.abs() {
        if mx >  DEAD { Dir::East }
        else if mx < -DEAD { Dir::West }
        else { Dir::None }
    } else {
        if my >  DEAD { Dir::North }
        else if my < -DEAD { Dir::South }
        else { Dir::None }
    }
}

/// World position of the player's local `(0,_,0)` corner given its
/// cell, motion direction, and `[0, 1)` progress toward the next cell.
/// `Dir::None` parks the player centred in `cell`.
///
/// The engine yaws actors around their volume's horizontal centre, so
/// we only need to position the local `(0,_,0)` corner — the centre of
/// the 5-wide prefab lands on the cell centre regardless of facing.
pub(crate) fn world_pos(cell: (u32, u32), dir: Dir, progress: f32) -> Vec3 {
    let (col, row) = cell;
    let (dc, dr) = dir.delta();
    let x = (col as f32 + 0.5 + dc as f32 * progress) * CELL as f32 + ORIGIN_X as f32;
    let z = (row as f32 + 0.5 + dr as f32 * progress) * CELL as f32 + ORIGIN_Z as f32;
    Vec3::new(
        x - PLAYER_W as f32 * 0.5,
        1.0,
        z - PLAYER_W as f32 * 0.5,
    )
}

/// World position of the player's geometric centre (the actor's
/// `position` is its local-origin corner; centre sits half a
/// `PLAYER_W` into both horizontal axes).
pub(crate) fn world_centre() -> Vec3 {
    let cell = unsafe { PLAYER_CELL };
    let dir = unsafe { PLAYER_DIR };
    let progress = unsafe { PLAYER_PROGRESS };
    let origin = world_pos(cell, dir, progress);
    Vec3::new(
        origin.x + PLAYER_W as f32 * 0.5,
        origin.y,
        origin.z + PLAYER_W as f32 * 0.5,
    )
}

/// Reset to spawn cell — used after a death or full restart.
pub(crate) fn reset_to_spawn() {
    unsafe {
        PLAYER_CELL = SPAWN_CELL;
        PLAYER_DIR = Dir::None;
        PLAYER_PROGRESS = 0.0;
        DESIRED_DIR = Dir::None;
        if let Some(actor) = PLAYER {
            actor_set_position(actor, world_pos(PLAYER_CELL, Dir::None, 0.0));
        }
    }
}
