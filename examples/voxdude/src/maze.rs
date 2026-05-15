//! Maze data + painting + spatial queries.
//!
//! The board is a 2D char grid baked into source (`MAZE`). Each cell
//! maps to a `CELL × CELL` patch of voxels at ground level, with walls
//! `WALL_H` voxels tall. Painting happens once at `init()` (and again
//! after a restart); spatial queries (`cell_open`, `dir_open`,
//! `wall_at`) run every frame from movement code.
//!
//! The dot grid (`DOT_GRID`) is the only mutable state here — it tracks
//! which dots/pellets are still uneaten so the game loop can clear them
//! cell-by-cell as the player passes over them.

use voxlconsl_sdk::*;

use crate::{
    M_DOT, M_FLOOR, M_POWER_PELLET, M_WALL, M_WALL_CAP, M_WALL_OUTER, M_WALL_PIP,
};

// ── Board dimensions ──────────────────────────────────────────────

pub(crate) const COLS: u32 = 28;
pub(crate) const ROWS: u32 = 31;

/// Voxels per maze cell. Sized so the camera-follow render scheme can
/// frame walls + dots + actors crisply without trying to fit the whole
/// 224×248-voxel world on a 256×144 canvas.
pub(crate) const CELL: u32 = 8;

/// Walls are deliberately short so the slight camera tilt in
/// `set_follow_camera` reads as depth without making the wall sides
/// dominate the frame.
pub(crate) const WALL_H: u32 = 2;

/// World x of cell column 0 (leaves a 1-cell margin around the board
/// so the player at the edge has visual breathing room).
pub(crate) const ORIGIN_X: u32 = CELL;
pub(crate) const ORIGIN_Z: u32 = CELL;

// ── Maze layout ───────────────────────────────────────────────────
//
// 31 rows × 28 cols. Legend:
//   `#`  wall
//   `.`  dot
//   `o`  power pellet
//   ` `  empty traversable cell (no dot)
//   `P`  player spawn (treated as empty for painting)
//   `G`  ghost spawn (treated as empty for painting)
//
// Every row must be exactly `COLS` characters wide — checked by the
// const-time assert below.
pub(crate) const MAZE: &[&[u8]] = &[
    b"############################",
    b"#............##............#",
    b"#.####.#####.##.#####.####.#",
    b"#o####.#####.##.#####.####o#",
    b"#.####.#####.##.#####.####.#",
    b"#..........................#",
    b"#.####.##.########.##.####.#",
    b"#.####.##.########.##.####.#",
    b"#......##....##....##......#",
    b"######.##### ## #####.######",
    b"     #.##### ## #####.#     ",
    b"     #.##          ##.#     ",
    b"     #.## ###  ### ##.#     ",
    b"######.## #GG GG # ##.######",
    b"      .   #      #   .      ",
    b"######.## ######## ##.######",
    b"     #.## ######## ##.#     ",
    b"     #.##          ##.#     ",
    b"     #.## ######## ##.#     ",
    b"######.## ######## ##.######",
    b"#............##............#",
    b"#.####.#####.##.#####.####.#",
    b"#.####.#####.##.#####.####.#",
    b"#o..##.......P........##..o#",
    b"###.##.##.########.##.##.###",
    b"###.##.##.########.##.##.###",
    b"#......##....##....##......#",
    b"#.##########.##.##########.#",
    b"#.##########.##.##########.#",
    b"#..........................#",
    b"############################",
];

const _: () = {
    assert!(MAZE.len() == ROWS as usize);
    let mut i = 0;
    while i < MAZE.len() {
        assert!(MAZE[i].len() == COLS as usize, "maze row width mismatch");
        i += 1;
    }
};

// ── Dot tracking ──────────────────────────────────────────────────
//
// `DOT_GRID[row][col]` holds 0 (empty), 1 (dot), or 2 (power pellet).
// Set during `paint_maze`; cleared cell-by-cell as the player eats
// each item. `DOTS_REMAINING` is the running countdown — hitting zero
// fires the win state.

pub(crate) static mut DOT_GRID: [[u8; COLS as usize]; ROWS as usize] =
    [[0; COLS as usize]; ROWS as usize];
pub(crate) static mut DOTS_REMAINING: u32 = 0;

// ── Cardinal directions ───────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum Dir { None, North, South, East, West }

impl Dir {
    pub(crate) fn delta(self) -> (i32, i32) {
        match self {
            Dir::None  => (0, 0),
            Dir::North => (0, -1),
            Dir::South => (0,  1),
            Dir::East  => (1,  0),
            Dir::West  => (-1, 0),
        }
    }
    pub(crate) fn opposite(self) -> Dir {
        match self {
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::East  => Dir::West,
            Dir::West  => Dir::East,
            Dir::None  => Dir::None,
        }
    }
}

// ── Painting ──────────────────────────────────────────────────────

/// Walk the maze once and emit voxels.
///
/// Wall cells get a `CELL × WALL_H × CELL` base block; corners /
/// T-junctions / end caps add one extra voxel of brighter "cap"
/// material on top; cells with walls in all four cardinals get a
/// glowing `+` pip on the top face. Open cells get a dim single-voxel
/// floor dot at the cell centre so corridors read as a tiled surface.
///
/// Initialises [`DOT_GRID`] and [`DOTS_REMAINING`] as a side effect.
pub(crate) fn paint_maze() {
    let mut dot_count = 0u32;
    for row in 0..ROWS {
        let line = MAZE[row as usize];
        for col in 0..COLS {
            let ch = line[col as usize];
            let x0 = ORIGIN_X + col * CELL;
            let z0 = ORIGIN_Z + row * CELL;
            match ch {
                b'#' => paint_wall_cell(col, row, x0, z0),
                b'.' => {
                    paint_floor_pip(x0, z0);
                    paint_dot(x0, z0, M_DOT, 1);
                    unsafe { DOT_GRID[row as usize][col as usize] = 1; }
                    dot_count += 1;
                }
                b'o' => {
                    paint_floor_pip(x0, z0);
                    paint_dot(x0, z0, M_POWER_PELLET, 2);
                    unsafe { DOT_GRID[row as usize][col as usize] = 2; }
                    dot_count += 1;
                }
                // space / 'P' / 'G' — open cell with no dot. Still
                // paint a floor pip so corridor texture stays
                // continuous through spawn and ghost-house cells.
                _ => paint_floor_pip(x0, z0),
            }
        }
    }
    unsafe { DOTS_REMAINING = dot_count; }
}

/// Subtle dim floor voxel at the centre of an open cell. Lives at
/// `y=0` so it sits under the player/ghosts/dots and reads as a quiet
/// grid stipple without competing with the playables.
fn paint_floor_pip(x0: u32, z0: u32) {
    let cx = x0 + CELL / 2;
    let cz = z0 + CELL / 2;
    set_voxel(UVec3::new(cx, 0, cz), M_FLOOR);
}

/// Classification of a wall cell — drives the visual trim painted on
/// top of the base block.
#[derive(Copy, Clone)]
struct WallRole {
    /// Cell sits on the maze's outer ring.
    outer: bool,
    /// Cell isn't part of a clean straight-line wall (it's a corner,
    /// T-junction, or end cap). Gets an extra voxel of cap material
    /// on top — the "pillar at every turn" look.
    cap: bool,
    /// Cell has wall neighbours in all four cardinals — only happens
    /// deep inside thicker (≥3-cell-wide) wall blocks. Gets a `+` pip
    /// of emissive pop colour on top.
    interior: bool,
}

fn paint_wall_cell(col: u32, row: u32, x0: u32, z0: u32) {
    let role = wall_role(col, row);
    let body = if role.outer { M_WALL_OUTER } else { M_WALL };
    fill_box(
        UVec3::new(x0, 0, z0),
        UVec3::new(x0 + CELL - 1, WALL_H - 1, z0 + CELL - 1),
        body,
    );

    let cx = x0 + CELL / 2;
    let cz = z0 + CELL / 2;
    if role.interior {
        // `+` pip on the top face of the wall — 5 voxels at y=WALL_H.
        set_voxel(UVec3::new(cx,     WALL_H, cz    ), M_WALL_PIP);
        set_voxel(UVec3::new(cx - 1, WALL_H, cz    ), M_WALL_PIP);
        set_voxel(UVec3::new(cx + 1, WALL_H, cz    ), M_WALL_PIP);
        set_voxel(UVec3::new(cx,     WALL_H, cz - 1), M_WALL_PIP);
        set_voxel(UVec3::new(cx,     WALL_H, cz + 1), M_WALL_PIP);
    } else if role.cap {
        // Full-cell cap layer one voxel tall — the wall now stands
        // `WALL_H+1` here. Reads as a brighter pillar at every turn or
        // branch in the maze.
        fill_box(
            UVec3::new(x0,             WALL_H, z0            ),
            UVec3::new(x0 + CELL - 1,  WALL_H, z0 + CELL - 1),
            M_WALL_CAP,
        );
    }
}

fn wall_role(col: u32, row: u32) -> WallRole {
    let outer = col == 0 || row == 0 || col == COLS - 1 || row == ROWS - 1;
    // `wall_at` treats out-of-bounds as *open* — without that, the
    // four maze corners would look like interior cells (4 walls)
    // instead of caps.
    let n = wall_at(col as i32,     row as i32 - 1);
    let s = wall_at(col as i32,     row as i32 + 1);
    let e = wall_at(col as i32 + 1, row as i32);
    let w = wall_at(col as i32 - 1, row as i32);
    let horiz_run = e && w;
    let vert_run  = n && s;
    let has_horiz = e || w;
    let has_vert  = n || s;
    let interior = horiz_run && vert_run;
    let straight = (horiz_run && !has_vert) || (vert_run && !has_horiz);
    WallRole { outer, cap: !interior && !straight, interior }
}

fn wall_at(col: i32, row: i32) -> bool {
    if col < 0 || row < 0 || col >= COLS as i32 || row >= ROWS as i32 {
        return false;
    }
    MAZE[row as usize][col as usize] == b'#'
}

/// Paint a dot or power pellet at the centre of a cell.
///
/// `kind == 1` paints a 2×2×2 cube (small chunky 3D blob); `kind == 2`
/// paints a 3×3×3 emissive cube (the power pellet — visibly larger and
/// taller than dots). The top-down camera sees only voxel tops, so the
/// chunky footprints make the dots readable at the canvas's ~2-pixels-
/// per-voxel sampling.
pub(crate) fn paint_dot(x0: u32, z0: u32, material: u8, kind: u32) {
    let cx = x0 + CELL / 2;
    let cz = z0 + CELL / 2;
    if kind <= 1 {
        fill_box(
            UVec3::new(cx - 1, 1, cz - 1),
            UVec3::new(cx,     2, cz    ),
            material,
        );
    } else {
        fill_box(
            UVec3::new(cx - 1, 1, cz - 1),
            UVec3::new(cx + 1, 3, cz + 1),
            material,
        );
    }
}

/// Remove a dot/pellet voxel from the world at the given cell. Mirrors
/// `paint_dot`'s footprint so we erase exactly what was painted.
/// `kind` is the [`DOT_GRID`] value (1 = dot, 2 = pellet).
pub(crate) fn clear_dot_cell(col: u32, row: u32, kind: u8) {
    let x0 = ORIGIN_X + col * CELL;
    let z0 = ORIGIN_Z + row * CELL;
    match kind {
        1 => paint_dot(x0, z0, 0, 1),
        2 => paint_dot(x0, z0, 0, 2),
        _ => {}
    }
}

// ── Spatial queries ───────────────────────────────────────────────

/// `true` iff cell `(col, row)` is inside the maze and walkable
/// (anything other than `#`). Out-of-bounds cells count as walls.
pub(crate) fn cell_open(col: i32, row: i32) -> bool {
    if col < 0 || row < 0 || col >= COLS as i32 || row >= ROWS as i32 {
        return false;
    }
    MAZE[row as usize][col as usize] != b'#'
}

/// `true` iff stepping from `cell` in `dir` lands on a walkable cell.
pub(crate) fn dir_open(cell: (u32, u32), dir: Dir) -> bool {
    if matches!(dir, Dir::None) { return false; }
    let (dc, dr) = dir.delta();
    cell_open(cell.0 as i32 + dc, cell.1 as i32 + dr)
}
