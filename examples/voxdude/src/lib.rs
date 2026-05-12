//! voxdude — a voxel pacman clone.
//!
//! Top-down voxel maze. The player roams a 28×31 grid eating dots
//! and dodging four ghosts; grabbing one of the four power pellets
//! briefly flips the ghosts into a frightened state and the player
//! can eat *them* instead. Clear every dot to win, lose three lives
//! to lose.
//!
//! The maze is a 2D char grid baked into source — see [`MAZE`] below.
//! Each cell maps to a `CELL × CELL` patch of voxels at ground level,
//! with walls `WALL_H` voxels tall and dots painted at mid-cell. The
//! camera looks down from above with a gentle tilt so depth reads.

#![no_std]
#![no_main]

use voxlconsl_sdk::*;
use voxlconsl_sdk::animation::Flipbook;
use voxlconsl_sdk::audio;

// ── Audio (§5) ────────────────────────────────────────────────────
// Two patches live in audio/patches.toml — the bundler packs them
// into the cart's Audio section and the host pre-populates the patch
// table before init() runs.

const PATCH_CHOMP: u8 = 0;
const PATCH_PING:  u8 = 1;

// MIDI note constants used by the various SFX triggers.
const NOTE_CHOMP:        u8 = 72; // C5 — short bright stab on each dot
const NOTE_POWER_PELLET: u8 = 76; // E5 — power-up
const NOTE_GHOST_EATEN:  u8 = 84; // C6 — bright reward note
const NOTE_DEATH:        u8 = 48; // C3 — low descending pang
const NOTE_WIN:          u8 = 79; // G5 — triumphant ping

// ── Material slots (must match materials.toml) ────────────────────
const M_WALL:              u8 = 1;
const M_FLOOR:             u8 = 2;
const M_DOT:               u8 = 3;
const M_POWER_PELLET:      u8 = 4;
const M_PLAYER:            u8 = 5;
const M_GHOST_BLINKY:      u8 = 6;
const M_GHOST_PINKY:       u8 = 7;
const M_GHOST_INKY:        u8 = 8;
const M_GHOST_CLYDE:       u8 = 9;
const M_GHOST_FRIGHTENED:  u8 = 10;
const M_PARTICLE:          u8 = 11;
const M_GHOST_EYE:         u8 = 12;
const M_GHOST_FLASH:       u8 = 13;
const M_WALL_OUTER:        u8 = 14;
const M_WALL_CAP:          u8 = 15;
const M_WALL_PIP:          u8 = 16;
const M_HUD_SCORE:         u8 = 17;
const M_HUD_OUTLINE:       u8 = 18;

// ── Maze geometry ─────────────────────────────────────────────────
//
// Classic-pacman 28×31 board. Each cell is a `CELL × CELL` voxel
// patch with walls `WALL_H` voxels tall. We can afford the full
// resolution because the camera follows the player at low altitude
// (see `render`) — no need to fit the whole board on screen.

const COLS: u32 = 28;
const ROWS: u32 = 31;
/// Voxels per maze cell. Up from 4 → 8: each cell now has enough
/// voxels to draw chunky walls, a clear dot, and the player/ghost
/// actors with room to spare. The camera-follow render scheme means
/// we no longer need to fit the entire 224×248-voxel world on screen.
const CELL: u32 = 8;
/// Walls are deliberately short (2 voxels) so the slight camera tilt
/// in `set_follow_camera` reads as depth without making the wall
/// sides dominate the frame.
const WALL_H: u32 = 2;
/// World x of cell column 0 (leaves a 1-cell margin so the player at
/// the edge of the maze has some breathing room visually).
const ORIGIN_X: u32 = CELL;
const ORIGIN_Z: u32 = CELL;
const WORLD_W:  u32 = ORIGIN_X + COLS * CELL + CELL; // 120
const WORLD_D:  u32 = ORIGIN_Z + ROWS * CELL + CELL; // 132

/// Maze layout — 21 rows × 19 columns. Legend:
///
/// - `#` wall
/// - `.` dot
/// - `o` power pellet
/// - ` ` empty traversable cell (no dot)
/// - `P` player spawn (treated as empty for painting purposes)
/// - `G` ghost spawn (same: empty cell, used as a marker)
///
/// Every row must be exactly `COLS` characters wide.
const MAZE: &[&[u8]] = &[
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

// Validated at compile time: maze dimensions match the constants.
const _: () = {
    assert!(MAZE.len() == ROWS as usize);
    let mut i = 0;
    while i < MAZE.len() {
        assert!(MAZE[i].len() == COLS as usize, "maze row width mismatch");
        i += 1;
    }
};

// ── Player state ──────────────────────────────────────────────────
//
// Grid-locked movement à la classic pacman: the player sits on a
// cell (`PLAYER_CELL`), moves toward an adjacent cell in `PLAYER_DIR`,
// and `PLAYER_PROGRESS ∈ [0, 1)` tracks the lerp from cell-centre to
// cell-centre. Direction changes are buffered in `DESIRED_DIR` and
// applied at the next cell boundary if the buffered direction is
// open. The 180° reverse is also allowed at any time (mid-cell).
//
// Player speed is in *cells per second* — 5.5 ≈ classic pacman feel
// at our voxel scale.

const PLAYER_W: u32 = 5;
const PLAYER_SPEED_CPS: f32 = 5.5;

// ── Pacman chomp animation ────────────────────────────────────────
//
// Two prefab frames — mouth closed and mouth open — cycled via the
// SDK `Flipbook` helper at 80 ms/frame whenever the player is
// moving. When stopped, the mouth snaps back closed. The actor's
// yaw rotates the prefab to face the current direction of motion;
// the prefab itself is authored facing `+x`.

const DUDE_W: usize = 5;
const DUDE_H: usize = 3;
const DUDE_D: usize = 5;
const DUDE_VOL: usize = DUDE_W * DUDE_H * DUDE_D;

const P_CHOMP_CLOSED: PrefabId = PrefabId(1);
const P_CHOMP_OPEN:   PrefabId = PrefabId(2);

static mut DENSE_CHOMP_0: [u8; DUDE_VOL] = [0; DUDE_VOL];
static mut DENSE_CHOMP_1: [u8; DUDE_VOL] = [0; DUDE_VOL];

const CHOMP_FRAMES: &[PrefabId] = &[P_CHOMP_CLOSED, P_CHOMP_OPEN];
static mut CHOMP_FB: Flipbook = Flipbook::new(CHOMP_FRAMES, 80, true);
static mut CURRENT_FRAME: PrefabId = P_CHOMP_CLOSED;

/// Player facing in radians (yaw around +Y). Updated whenever the
/// player has a current direction; preserved while stopped so the
/// dude keeps facing the way he last moved.
static mut PLAYER_FACING: f32 = 0.0;


#[derive(Copy, Clone, PartialEq, Eq)]
enum Dir { None, North, South, East, West }

impl Dir {
    fn delta(self) -> (i32, i32) {
        match self {
            Dir::None  => (0, 0),
            Dir::North => (0, -1),
            Dir::South => (0,  1),
            Dir::East  => (1,  0),
            Dir::West  => (-1, 0),
        }
    }
    fn opposite(self) -> Dir {
        match self {
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::East  => Dir::West,
            Dir::West  => Dir::East,
            Dir::None  => Dir::None,
        }
    }
}

static mut PLAYER:          Option<ActorId> = None;
static mut PLAYER_CELL:     (u32, u32)      = (13, 23); // matches the `P` in MAZE
static mut PLAYER_DIR:      Dir             = Dir::None;
static mut PLAYER_PROGRESS: f32             = 0.0;
static mut DESIRED_DIR:     Dir             = Dir::None;
static mut MOVE_ACTION:     ActionHandle    = ActionHandle(0);

// ── Score state ───────────────────────────────────────────────────
//
// `DOT_GRID[row][col]` holds 0 (empty), 1 (dot), or 2 (power pellet).
// Painted in `paint_maze` from the same scan that emits voxels, then
// cleared cell-by-cell as the player eats them. `DOTS_REMAINING` is
// the running countdown — when it hits zero, the player wins.

const DOT_VALUE: u32 = 10;
const POWER_PELLET_VALUE: u32 = 50;
const GHOST_VALUE: u32 = 200;

static mut DOT_GRID: [[u8; COLS as usize]; ROWS as usize] =
    [[0; COLS as usize]; ROWS as usize];
static mut DOTS_REMAINING: u32 = 0;
static mut SCORE: u32 = 0;
static mut LIVES: u32 = 3;

// ── Ghosts ────────────────────────────────────────────────────────
//
// Four ghosts with classic-ish personalities. Each picks a direction
// at every cell boundary by selecting the neighbour (excluding the
// 180°-reverse) that minimises manhattan distance to its personality
// target. In frightened mode (kicked on by power pellets) the target
// logic is replaced with a host-`rand`-driven coin flip among open
// directions, the speed drops, and the colour swaps to blue.

const GHOST_W: u32 = 5;
const GHOST_SPEED_CPS: f32 = 4.5;
const GHOST_FRIGHTENED_SPEED_CPS: f32 = 3.0;
const FRIGHTENED_MS: u32 = 7_000;
/// Cells per second is the player's max speed; this is the threshold
/// below which `dir_open` is allowed to switch even without input.
const SCATTER_DISTANCE: u32 = 8;

/// Per-ghost squash/stretch body wobble. Each ghost flips between
/// two slightly different silhouettes every `WOBBLE_FRAME_MS` so the
/// crew reads as "alive" instead of "static blocks".
const WOBBLE_FRAME_MS: u32 = 150;
/// When `FRIGHTENED_MS_LEFT` drops below this we start the
/// classic-pacman white-flash tell. `FLASH_PERIOD_MS` is the
/// half-period (i.e. the duration of one solid colour).
const FLASH_WINDOW_MS: u32 = 2_000;
const FLASH_PERIOD_MS: u32 = 200;
/// "Never painted" sentinel for the per-ghost paint cache, so the
/// first repaint after init always fires regardless of what colour
/// happens to live in `paint_mat`.
const PAINT_PHASE_NEVER: u8 = 255;

#[derive(Copy, Clone, PartialEq, Eq)]
enum GhostKind { Blinky, Pinky, Inky, Clyde }

#[derive(Copy, Clone)]
struct Ghost {
    actor: Option<ActorId>,
    kind:  GhostKind,
    home:  (u32, u32),
    cell:  (u32, u32),
    dir:   Dir,
    progress: f32,
    /// Per-ghost frightened flag. Mirrors `FRIGHTENED_MS_LEFT > 0`
    /// except that an *eaten* ghost clears its own flag while the
    /// timer keeps running for the rest of the crew — that's how
    /// the eaten ghost gets its personality colour back even while
    /// the others stay blue.
    frightened:   bool,
    /// Body-wobble timer + phase. `wobble_ms` accumulates; every
    /// `WOBBLE_FRAME_MS` it flips `wobble_phase` between 0 and 1.
    wobble_ms:    u32,
    wobble_phase: u8,
    /// Cached visual state from the last `repaint_ghost` call.
    /// `paint_phase == PAINT_PHASE_NEVER` forces a repaint on the
    /// first tick.
    paint_dir:    Dir,
    paint_mat:    u8,
    paint_phase:  u8,
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

const GHOST_COUNT: usize = 4;

// Spawn cells line up with the four `G` markers in MAZE row 13 (the
// ghost house interior — cols 11, 12, 14, 15). Ghosts navigate
// north toward the maze proper from there.
// Stagger the wobble seed offsets so the four ghosts don't pulse in
// perfect lockstep — gives the crew a bit of organic life.
static mut GHOSTS: [Ghost; GHOST_COUNT] = [
    ghost_init(GhostKind::Blinky, (11, 13), Dir::North,   0),
    ghost_init(GhostKind::Pinky,  (12, 13), Dir::North,  40),
    ghost_init(GhostKind::Inky,   (14, 13), Dir::North,  80),
    ghost_init(GhostKind::Clyde,  (15, 13), Dir::North, 120),
];

static mut FRIGHTENED_MS_LEFT: u32 = 0;

// ── Particles ─────────────────────────────────────────────────────
//
// Tiny chomp-burst sparkles emitted on every dot pickup. Each
// particle is a 2×2×2 cube actor; we pool a fixed cap, hide them
// when inactive, and run a cart-side gravity + TTL integrator each
// `update`. No collision — they just arc through the air and pop
// out at end-of-life.
const PARTICLE_CAP:        usize = 24;
const PARTICLES_PER_BURST: usize = 5;
const PARTICLE_TTL_MS:     u32   = 700;
const PARTICLE_GRAVITY:    f32   = 0.040;
const PARTICLE_W:          u32   = 2;
const PARTICLE_VOL:        usize = 2 * 2 * 2;
const P_PARTICLE:          PrefabId = PrefabId(3);

#[derive(Copy, Clone)]
struct Particle {
    actor:    Option<ActorId>,
    pos:      Vec3,
    vel:      Vec3,
    ttl_ms:   u32,
    active:   bool,
}

static mut PARTICLES: [Particle; PARTICLE_CAP] = [Particle {
    actor:  None,
    pos:    Vec3 { x: 0.0, y: 0.0, z: 0.0 },
    vel:    Vec3 { x: 0.0, y: 0.0, z: 0.0 },
    ttl_ms: 0,
    active: false,
}; PARTICLE_CAP];

static mut DENSE_PARTICLE: [u8; PARTICLE_VOL] = [M_PARTICLE; PARTICLE_VOL];

#[derive(Copy, Clone, PartialEq, Eq)]
enum GameState { Playing, Won, Lost }

static mut STATE: GameState = GameState::Playing;
static mut RESTART_ACTION: ActionHandle = ActionHandle(0);

// ── HUD (camera-relative) ─────────────────────────────────────────
//
// Score and lives ride on dedicated actors that get re-positioned
// each frame relative to the player's world centre. The camera is
// rigidly attached to the player (`set_follow_camera`), so a fixed
// world-space offset from the player produces a fixed *screen-space*
// HUD position. Score is rasterised straight into the actor's volume
// via FONT_ANSI bits on every score change. Lives are pre-spawned
// player-colour cube actors that toggle visibility based on the
// current life count.
const HUD_Y: f32 = 10.0;
const SCORE_GLYPH_W:   u32 = 10;            // FONT_ANSI cell_w
const SCORE_GLYPH_H:   u32 = 11;            // FONT_ANSI cell_h
/// Voxel pad around each digit reserved for the dark outline halo
/// painted under the bright digit. 1 voxel border on every side.
const SCORE_PAD:       u32 = 1;
/// Each digit gets its own actor of this footprint. We split per
/// digit because the host's prefab chunk size caps at 32, so a
/// single 6-digit actor (62 voxels wide) won't fit — one digit
/// (12 voxels wide) easily does. Digits are placed at integer
/// `SCORE_DIGIT_W` strides so adjacent halos meet exactly without
/// overlapping or gapping.
const SCORE_DIGIT_W:   u32 = SCORE_GLYPH_W + 2 * SCORE_PAD;
const SCORE_DIGIT_D:   u32 = SCORE_GLYPH_H + 2 * SCORE_PAD;
const SCORE_DIGIT_VOL: usize = (SCORE_DIGIT_W * 1 * SCORE_DIGIT_D) as usize;
const SCORE_MAX_DIGITS: usize = 6;
/// North edge of the score row (z offset south of the player). The
/// row's south edge sits `SCORE_DIGIT_D` further south.
const HUD_OFFSET_S:    f32 = 14.0;
/// Lives' south edge lines up with the score's south edge so they
/// share a single visual bottom row.
const LIVES_OFFSET_S:  f32 = HUD_OFFSET_S + (SCORE_DIGIT_D - LIFE_W) as f32;
/// Left edge of the leftmost score digit, measured from player
/// centre.
const SCORE_LEFT_OFFSET:  f32 = 40.0;
/// Right edge of the rightmost life icon, measured from player
/// centre.
const LIVES_RIGHT_OFFSET: f32 = 40.0;
const LIFE_W:          u32 = 3;
const LIFE_VOL:        usize = (LIFE_W * LIFE_W * LIFE_W) as usize;
const LIFE_SPACING:    f32 = 6.0;
const LIFE_ACTOR_COUNT: usize = 3;
const P_LIFE:        PrefabId = PrefabId(4);
const P_SCORE_DIGIT: PrefabId = PrefabId(5);

static mut SCORE_DIGIT_ACTORS: [Option<ActorId>; SCORE_MAX_DIGITS] =
    [None; SCORE_MAX_DIGITS];
static mut SCORE_LAST_DRAWN:   u32 = u32::MAX;
/// Number of digits currently visible — `tick_hud` reads this to skip
/// repositioning the hidden right-side digits.
static mut SCORE_LAST_DIGITS:  u32 = 1;
static mut LIFE_ACTORS: [Option<ActorId>; LIFE_ACTOR_COUNT] = [None; LIFE_ACTOR_COUNT];
static mut DENSE_LIFE:        [u8; LIFE_VOL] = [M_PLAYER; LIFE_VOL];
/// All-air prefab whose dimensions match one score digit's volume.
/// `actor_spawn` would hand back a fixed 16-cube `OwnedVolume`, so any
/// `actor_set_voxel` past column 15 would silently no-op. Spawning
/// from this prefab makes the fork-on-mutate path inherit the right
/// 12×1×13 size.
static mut DENSE_SCORE_DIGIT: [u8; SCORE_DIGIT_VOL] = [0; SCORE_DIGIT_VOL];

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    // Space-cabinet sky — a saturated purple void around the maze
    // so the screen reads "you're inside an arcade backdrop", not
    // just floating in black. The renderer currently only uses the
    // `top` parameter (`horizon` is plumbed but not yet sampled), so
    // we pick a single mid-purple that clearly distinguishes the
    // sky from the deep-blue walls. Fog would help corridors fade
    // out beyond the camera frame, but the SDK doesn't expose
    // `camera_set_fog` yet — flagged for later.
    sky_set_gradient(
        Material::pack_color(8, 1), // purple:1 — vivid purple ceiling
        Material::pack_color(8, 0), // purple:0 — placeholder horizon for when the renderer
                                    //            grows a real gradient sampler.
    );
    // Top-down lighting — sun straight down so wall tops are lit and
    // the floor space stays dark and quiet.
    light_set_sun(Vec3::new(0.0, -1.0, 0.0), 0, 0);

    paint_maze();

    // Camera follows the player each frame from `render`. Set the FoV
    // once here; the per-frame look-at update lives in
    // `set_follow_camera`.
    camera_set_fov(60.0);
    set_follow_camera();

    // Player actor — pacman-style chomper baked as two prefab frames
    // (mouth closed / open) and cycled via the flipbook helper.
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
        actor_set_position(id, player_world_pos(PLAYER_CELL, Dir::None, 0.0));
    }

    // Ghost actors — each is a 5×5×5 actor volume the cart repaints
    // per-frame to drive body wobble + eyes + frightened flash. See
    // `tick_ghost_visuals` for the actual paint logic; the initial
    // repaint happens on the first `update` tick.
    unsafe {
        for i in 0..GHOST_COUNT {
            let id = actor_spawn().expect("failed to spawn ghost");
            GHOSTS[i].actor = Some(id);
            // Eyes face the spawn direction so they don't look
            // sideways during the very first frame before update
            // runs.
            GHOSTS[i].paint_dir = GHOSTS[i].dir;
            repaint_ghost(
                id,
                ghost_color(GHOSTS[i].kind),
                GHOSTS[i].dir,
                GHOSTS[i].wobble_phase,
            );
            GHOSTS[i].paint_mat = ghost_color(GHOSTS[i].kind);
            GHOSTS[i].paint_phase = GHOSTS[i].wobble_phase;
            actor_set_position(id, ghost_world_pos(GHOSTS[i].cell, Dir::None, 0.0));
        }
    }

    // Particle pool — pre-spawn N inactive actors and reuse them
    // for every chomp-burst so we never allocate during play.
    unsafe { init_particles(); }

    // HUD actors (score + lives). `tick_hud` repositions them every
    // frame so they ride the camera-follow rig.
    unsafe { init_hud(); }

    // ── Input ─────────────────────────────────────────────────────
    // Reuse the standard PrimaryMovement Axis2D binding (WASD on
    // browser). We quantise it to a cardinal direction inside
    // `update` so gamepad sticks work too.
    unsafe {
        MOVE_ACTION = input_declare_action(
            ActionKind::Axis2D, BindingHint::PrimaryMovement, "move",
        );
        // R (mapped via PrimaryFire's default binding chain — `J` on
        // the browser host — but we want a more conventional restart
        // button). The host's binding-hint table doesn't carry a
        // dedicated "restart" hint, so we reuse `SecondaryFire` which
        // defaults to `K`. Pressing `K` after winning or losing the
        // game starts a fresh round.
        RESTART_ACTION = input_declare_action(
            ActionKind::Button, BindingHint::SecondaryFire, "restart",
        );
    }

}

/// One-time spawn of the score actor + 3 life-icon actors. The
/// score actor's volume size is fixed to fit the maximum digit
/// count; we paint into it per-update when the score changes. The
/// life actors share a single 3×3×3 player-colour prefab via CoW.
unsafe fn init_hud() {
    // Score digit prefab — one all-air 12×1×13 volume. We spawn
    // `SCORE_MAX_DIGITS` actor instances; each holds one digit glyph
    // plus the dark halo around it, and is positioned at integer
    // `SCORE_DIGIT_W` offsets so adjacent halos butt up cleanly.
    prefab_define(
        P_SCORE_DIGIT,
        unsafe { &*(&raw const DENSE_SCORE_DIGIT) },
        U8Vec3::new(SCORE_DIGIT_W as u8, 1, SCORE_DIGIT_D as u8),
    );
    for i in 0..SCORE_MAX_DIGITS {
        let id = actor_spawn_from(P_SCORE_DIGIT, Orientation::Up)
            .expect("score digit actor spawn");
        actor_set_visible(id, false);
        unsafe { SCORE_DIGIT_ACTORS[i] = Some(id); }
    }

    // Life icons — one prefab, three instances. The prefab CoW means
    // all three share one baked volume.
    prefab_define(
        P_LIFE,
        unsafe { &*(&raw const DENSE_LIFE) },
        U8Vec3::new(LIFE_W as u8, LIFE_W as u8, LIFE_W as u8),
    );
    for i in 0..LIFE_ACTOR_COUNT {
        let id = actor_spawn_from(P_LIFE, Orientation::Up)
            .expect("life actor spawn");
        unsafe { LIFE_ACTORS[i] = Some(id); }
    }
}

/// Rasterise `score` into the score actor using FONT_ANSI bits. Walks
/// the digits most-significant first so the leftmost glyph is the
/// highest place. The actor's local +x axis runs east (screen-right)
/// and local +z axis runs south, so we paint with glyph row 0 at
/// low z — that places the top of each letter at the screen's top
/// (north).
fn repaint_score(score: u32) {
    // Decode into a fixed-size stack buffer, most-significant-first.
    let mut digits = [0u8; SCORE_MAX_DIGITS];
    let len: usize;
    if score == 0 {
        digits[0] = b'0';
        len = 1;
    } else {
        let mut tmp = [0u8; SCORE_MAX_DIGITS];
        let mut tlen = 0usize;
        let mut n = score;
        while n > 0 && tlen < SCORE_MAX_DIGITS {
            tmp[tlen] = b'0' + (n % 10) as u8;
            n /= 10;
            tlen += 1;
        }
        for i in 0..tlen {
            digits[i] = tmp[tlen - 1 - i];
        }
        len = tlen;
    }
    unsafe { SCORE_LAST_DIGITS = len as u32; }

    for i in 0..SCORE_MAX_DIGITS {
        let actor = match unsafe { SCORE_DIGIT_ACTORS[i] } {
            Some(a) => a,
            None => continue,
        };
        actor_clear(actor);
        if i >= len {
            continue;
        }
        paint_score_digit(actor, digits[i]);
    }
}

/// Paint one digit glyph + halo into a single-digit score actor.
fn paint_score_digit(actor: ActorId, ch: u8) {
    let font = &text::FONT_ANSI;
    let cell_w = font.cell_width() as u32;
    let cell_h = font.cell_height() as u32;

    // Two-pass paint: outline halo first, then the bright glyph.
    // Pass 1 paints the 8 Moore neighbours of every lit bit so a
    // continuous 1-voxel dark border surrounds the stroke. Pass 2
    // overwrites the lit bits themselves with the bright score
    // colour, so glyph pixels never carry the halo colour.
    for row in 0..cell_h {
        for col in 0..cell_w {
            if !font.glyph_bit(ch as u32, col as u8, row as u8) {
                continue;
            }
            let cx = col + SCORE_PAD;
            let cz = row + SCORE_PAD;
            for dz in 0i32..=2 {
                for dx in 0i32..=2 {
                    if dx == 1 && dz == 1 { continue; }
                    let x = cx as i32 + dx - 1;
                    let z = cz as i32 + dz - 1;
                    if x < 0 || z < 0
                        || x >= SCORE_DIGIT_W as i32
                        || z >= SCORE_DIGIT_D as i32
                    {
                        continue;
                    }
                    actor_set_voxel(
                        actor,
                        U8Vec3::new(x as u8, 0, z as u8),
                        M_HUD_OUTLINE,
                    );
                }
            }
        }
    }
    for row in 0..cell_h {
        for col in 0..cell_w {
            if !font.glyph_bit(ch as u32, col as u8, row as u8) {
                continue;
            }
            let x = col + SCORE_PAD;
            let z = row + SCORE_PAD;
            actor_set_voxel(actor, U8Vec3::new(x as u8, 0, z as u8), M_HUD_SCORE);
        }
    }
}

fn tick_hud() {
    // Both `tick_hud` and `set_follow_camera` call this from the
    // same frame after all player-state updates have committed, so
    // the HUD and the camera share an exact-equal `centre`. That
    // makes `origin - position` in the renderer's
    // `world_to_local_ray` cancel cleanly and the HUD lines up
    // with the camera frustum without drift.
    let centre = player_world_centre();
    let score = unsafe { SCORE };
    let lives = unsafe { LIVES };
    if score != unsafe { SCORE_LAST_DRAWN } {
        repaint_score(score);
        unsafe { SCORE_LAST_DRAWN = score; }
    }
    // Position each digit actor at a fixed integer offset from the
    // shared score origin. Hidden actors (beyond the digit count) get
    // toggled off so the cleared volume doesn't get rendered as a
    // ghost rectangle.
    let score_origin_x = centre.x - SCORE_LEFT_OFFSET;
    let score_z = centre.z + HUD_OFFSET_S;
    let visible_digits = unsafe { SCORE_LAST_DIGITS } as usize;
    for i in 0..SCORE_MAX_DIGITS {
        let actor = match unsafe { SCORE_DIGIT_ACTORS[i] } {
            Some(a) => a,
            None => continue,
        };
        let visible = i < visible_digits;
        actor_set_visible(actor, visible);
        if visible {
            let x = score_origin_x + (i as u32 * SCORE_DIGIT_W) as f32;
            actor_set_position(actor, Vec3::new(x, HUD_Y, score_z));
        }
    }

    // Lives — right-anchored row at the screen's bottom-right. The
    // *rightmost* cube always sits at `player.x + LIVES_RIGHT_OFFSET`;
    // the row extends leftward by `LIFE_SPACING` per slot. We hide
    // icons from the LEFT as lives are lost, so the remaining lives
    // stay anchored to the right edge (classic arcade tell — easier
    // to read than a row that visually drifts). Lives ride at
    // `LIVES_OFFSET_S` so their south edge lines up with the score's
    // south edge, sharing a single bottom row.
    let right_edge = centre.x + LIVES_RIGHT_OFFSET;
    let lives_z = centre.z + LIVES_OFFSET_S;
    for i in 0..LIFE_ACTOR_COUNT {
        let actor = match unsafe { LIFE_ACTORS[i] } {
            Some(a) => a,
            None => continue,
        };
        let from_right = (LIFE_ACTOR_COUNT - 1 - i) as u32;
        let visible = from_right < lives;
        actor_set_visible(actor, visible);
        if visible {
            let x0 = right_edge - LIFE_W as f32 - from_right as f32 * LIFE_SPACING;
            actor_set_position(actor, Vec3::new(x0, HUD_Y, lives_z));
        }
    }
}

/// Advance the chomp flipbook and rotate the player to face the
/// current direction of motion. Called every frame from `update`.
unsafe fn advance_chomp(actor: ActorId, dir: Dir, dt_ms: u32) {
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
        // Yaw rotates the prefab around +Y. Prefab is authored
        // facing `+x` (east) so east → 0 rad. Going clockwise (+yaw)
        // turns toward south (+z) under voxlconsl's right-handed
        // convention.
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

/// Bake one chomp frame into `dense`. The voxels live on a 5×3×5
/// grid (x-fastest, then y, then z — matches `prefab_define`'s
/// expected layout). When `open == true` a triangular wedge is cut
/// out of the `+x` face to form the mouth.
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

/// Paint a ghost's 5×5×5 actor volume: scallop-bottom body, head
/// dome, eyes pointing in `dir`, and a 1-voxel squash on the
/// alternate `phase` so the body subtly wobbles between frames.
/// Called from `tick_ghost_visuals` whenever any of (colour, dir,
/// phase) actually changes — caching keeps the per-frame host
/// traffic flat when nothing's moved.
fn repaint_ghost(actor: ActorId, body: u8, dir: Dir, phase: u8) {
    actor_clear(actor);

    // ── Body column ──────────────────────────────────────────────
    // y=0 is the scalloped "skirt" (handled below); the dome sits
    // above it. Phase 1 squashes the dome by 1 voxel — visible as
    // a gentle bob from the high camera.
    let body_top: u8 = if phase == 0 { 4 } else { 3 };
    actor_fill_box(
        actor,
        U8Vec3::new(0, 1, 0),
        U8Vec3::new(GHOST_W as u8 - 1, body_top, GHOST_W as u8 - 1),
        body,
    );

    // ── Skirt scallop on y=0 ─────────────────────────────────────
    // Two interleaving patterns — alternate with `phase` so the
    // ghost looks like it's walking on a row of little legs.
    let skirt_xs: [u8; 3] = if phase == 0 { [0, 2, 4] } else { [1, 3, 4] };
    for &x in &skirt_xs {
        actor_set_voxel(actor, U8Vec3::new(x, 0, 0), body);
        actor_set_voxel(actor, U8Vec3::new(x, 0, 2), body);
        actor_set_voxel(actor, U8Vec3::new(x, 0, 4), body);
    }

    // ── Eyes ─────────────────────────────────────────────────────
    // Two voxels on the leading face. The "leading face" is the
    // +x/-x/+z/-z face matching the ghost's chase direction; the
    // eyes get painted on the body surface so they read as a tiny
    // pacman-style face from the top-down view.
    paint_ghost_eyes(actor, dir, body_top);
}

/// Paint two darker eye voxels onto the face of the ghost body that
/// matches `dir`. `body_top` is the topmost solid y on the dome — we
/// place the eyes one voxel below the top so they don't disappear
/// during the squash frame.
fn paint_ghost_eyes(actor: ActorId, dir: Dir, body_top: u8) {
    let eye_y = body_top.saturating_sub(1).max(2);
    let (a, b) = match dir {
        Dir::East  => (U8Vec3::new(4, eye_y, 1), U8Vec3::new(4, eye_y, 3)),
        Dir::West  => (U8Vec3::new(0, eye_y, 1), U8Vec3::new(0, eye_y, 3)),
        Dir::North => (U8Vec3::new(1, eye_y, 0), U8Vec3::new(3, eye_y, 0)),
        Dir::South => (U8Vec3::new(1, eye_y, 4), U8Vec3::new(3, eye_y, 4)),
        // Stationary in-house: face east by default so the eyes
        // still read instead of vanishing into the body.
        Dir::None  => (U8Vec3::new(4, eye_y, 1), U8Vec3::new(4, eye_y, 3)),
    };
    actor_set_voxel(actor, a, M_GHOST_EYE);
    actor_set_voxel(actor, b, M_GHOST_EYE);
}

fn ghost_color(kind: GhostKind) -> u8 {
    match kind {
        GhostKind::Blinky => M_GHOST_BLINKY,
        GhostKind::Pinky  => M_GHOST_PINKY,
        GhostKind::Inky   => M_GHOST_INKY,
        GhostKind::Clyde  => M_GHOST_CLYDE,
    }
}

/// World position of a ghost (same convention as the player).
fn ghost_world_pos(cell: (u32, u32), dir: Dir, progress: f32) -> Vec3 {
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

/// World position of the player given its grid cell, the direction
/// it's moving in, and the [0, 1) progress toward the next cell. With
/// `dir == Dir::None`, the player just sits centred in `cell`.
///
/// The engine yaws the actor around its volume's horizontal centre,
/// so the cart only needs to position the local `(0,_,0)` corner so
/// the centre of the 5-wide prefab lands on the cell centre — the
/// dude stays put no matter which way he's facing.
fn player_world_pos(cell: (u32, u32), dir: Dir, progress: f32) -> Vec3 {
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

/// `true` iff cell `(col, row)` is inside the maze and walkable
/// (anything other than a wall). Out-of-bounds cells count as walls.
fn cell_open(col: i32, row: i32) -> bool {
    if col < 0 || row < 0 || col >= COLS as i32 || row >= ROWS as i32 {
        return false;
    }
    let ch = MAZE[row as usize][col as usize];
    ch != b'#'
}

/// `true` iff stepping from `cell` in `dir` lands on a walkable cell.
fn dir_open(cell: (u32, u32), dir: Dir) -> bool {
    if matches!(dir, Dir::None) { return false; }
    let (dc, dr) = dir.delta();
    cell_open(cell.0 as i32 + dc, cell.1 as i32 + dr)
}

/// Map an Axis2D reading to a cardinal direction. Dominant-axis wins
/// (so a slightly off-axis WASD press still resolves cleanly), with a
/// dead zone so a sticky gamepad doesn't spam direction changes.
fn quantise_axis(mx: f32, my: f32) -> Dir {
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

/// Walk the maze grid once and emit voxels.
///
/// Wall cells get a base `CELL × WALL_H × CELL` block, plus optional
/// trim driven by `wall_role`: the **outer perimeter** uses a brighter
/// body material, **corner / T-junction / end** cells bump one voxel
/// taller with a brighter cap, and **interior** cells (all four
/// cardinals are walls) get a glowing `+` pip on top. Open cells get a
/// dim single-voxel floor dot at the cell centre so corridors read as
/// a tiled surface rather than a void.
fn paint_maze() {
    let mut row = 0u32;
    let mut dot_count = 0u32;
    while row < ROWS {
        let line = MAZE[row as usize];
        let mut col = 0u32;
        while col < COLS {
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
                _ => {
                    // space / 'P' / 'G' — open cell with no dot; still
                    // paint the floor pip so the corridor texture is
                    // continuous through spawn/ghost-house cells.
                    paint_floor_pip(x0, z0);
                }
            }
            col += 1;
        }
        row += 1;
    }
    unsafe { DOTS_REMAINING = dot_count; }
}

/// Subtle dim floor voxel at the centre of an open cell. Lives at
/// `y=0` so it sits under the player/ghosts (which spawn at y=1) and
/// under dots/pellets (which start at y=1). Reads as a quiet grid
/// stipple from above without competing with the playables.
fn paint_floor_pip(x0: u32, z0: u32) {
    let cx = x0 + CELL / 2;
    let cz = z0 + CELL / 2;
    set_voxel(UVec3::new(cx, 0, cz), M_FLOOR);
}

/// Classification of a wall cell — drives the visual trim painted on
/// top of the base block. See `wall_role` for the underlying logic.
#[derive(Copy, Clone)]
struct WallRole {
    /// Cell sits on the maze's outer ring.
    outer: bool,
    /// Cell isn't part of a clean straight-line wall (it's a corner,
    /// T-junction, or end cap). Gets an extra voxel of cap material
    /// on top — that's the "contouring" / pillar look.
    cap: bool,
    /// Cell has wall neighbours in all four cardinal directions —
    /// only happens deep inside thicker (≥3-cell-wide) wall blocks.
    /// Gets a `+` pip of emissive pop colour on top.
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
        // `+` pip on the top face of the wall — 5 voxels at y=WALL_H,
        // bumping the cell to a `WALL_H+1` silhouette only at the
        // centre, leaving the rest of the wall top flat.
        set_voxel(UVec3::new(cx,     WALL_H, cz    ), M_WALL_PIP);
        set_voxel(UVec3::new(cx - 1, WALL_H, cz    ), M_WALL_PIP);
        set_voxel(UVec3::new(cx + 1, WALL_H, cz    ), M_WALL_PIP);
        set_voxel(UVec3::new(cx,     WALL_H, cz - 1), M_WALL_PIP);
        set_voxel(UVec3::new(cx,     WALL_H, cz + 1), M_WALL_PIP);
    } else if role.cap {
        // Full-cell cap layer one voxel tall — the wall now stands
        // `WALL_H+1` here. Read top-down as a brighter "pillar" at
        // every turn or branch in the maze.
        fill_box(
            UVec3::new(x0,     WALL_H, z0    ),
            UVec3::new(x0 + CELL - 1, WALL_H, z0 + CELL - 1),
            M_WALL_CAP,
        );
    }
}

fn wall_role(col: u32, row: u32) -> WallRole {
    let outer = col == 0 || row == 0 || col == COLS - 1 || row == ROWS - 1;
    // `wall_at` treats out-of-bounds as *open* — without that, the
    // four maze corners would look like interior cells (4 walls)
    // instead of caps.
    let n = wall_at(col as i32, row as i32 - 1);
    let s = wall_at(col as i32, row as i32 + 1);
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

/// Remove a dot/power-pellet voxel from the world at the given cell.
/// Mirrors `paint_dot`'s footprint so we erase exactly what was
/// painted. `kind` is the [`DOT_GRID`] value (1 = dot, 2 = pellet).
fn clear_dot_cell(col: u32, row: u32, kind: u8) {
    let x0 = ORIGIN_X + col * CELL;
    let z0 = ORIGIN_Z + row * CELL;
    match kind {
        1 => paint_dot(x0, z0, 0, 1),
        2 => paint_dot(x0, z0, 0, 2),
        _ => {}
    }
}


/// Paint a dot or power pellet at the centre of a cell.
///
/// `kind` matches the [`DOT_GRID`] discriminant — 1 paints a small
/// floor patch (2×1×2 voxels), 2 paints a bigger glowing cube
/// (2×2×2). The top-down camera sees only voxel tops, so bigger
/// footprints make the dots actually readable at the canvas's ~2-
/// pixels-per-voxel sampling.
fn paint_dot(x0: u32, z0: u32, material: u8, kind: u32) {
    let cx = x0 + CELL / 2;
    let cz = z0 + CELL / 2;
    if kind <= 1 {
        // 2×2×2 cube — small chunky 3D blob at cell centre. Sits at
        // y=1..2, *taller* than the 2-voxel walls so the dot pops
        // above the floor outline from any angle.
        fill_box(
            UVec3::new(cx - 1, 1, cz - 1),
            UVec3::new(cx, 2, cz),
            material,
        );
    } else {
        // Power pellet — bigger 3×3×3 emissive cube. Visibly larger
        // than dots and rises an extra voxel above wall height.
        fill_box(
            UVec3::new(cx - 1, 1, cz - 1),
            UVec3::new(cx + 1, 3, cz + 1),
            material,
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    let dt = (dt_ms as f32) / 1000.0;

    // End-of-game state freezes movement; the restart button (K)
    // brings the cart back to a fresh round. We still tick the HUD
    // so the score/lives stay visible during the freeze.
    if !matches!(unsafe { STATE }, GameState::Playing) {
        if input_action_pressed(unsafe { RESTART_ACTION }) {
            restart_game();
        }
        tick_hud();
        return;
    }

    // ── Read input + buffer the desired direction ────────────────
    let (mx, my) = input_action_axis2d(unsafe { MOVE_ACTION });
    let want = quantise_axis(mx, my);
    if !matches!(want, Dir::None) {
        unsafe { DESIRED_DIR = want; }
    }

    let cell = unsafe { PLAYER_CELL };
    let mut dir = unsafe { PLAYER_DIR };
    let desired = unsafe { DESIRED_DIR };
    let mut progress = unsafe { PLAYER_PROGRESS };

    // 180° reverse anywhere — invert direction and the progress
    // (so the player keeps the same sub-cell offset, just going the
    // other way).
    if !matches!(desired, Dir::None) && desired == dir.opposite() {
        dir = desired;
        progress = 1.0 - progress;
    }

    // Stopped + can start moving in the desired direction.
    if matches!(dir, Dir::None) && dir_open(cell, desired) {
        dir = desired;
        progress = 0.0;
    }

    // Advance progress; cross cells as needed (loop in case the frame
    // budget is long enough to cross more than one cell — pathological
    // but cheap to handle).
    let step = PLAYER_SPEED_CPS * dt;
    progress += step;
    let mut current_cell = cell;
    while progress >= 1.0 && !matches!(dir, Dir::None) {
        let (dc, dr) = dir.delta();
        let next_col = current_cell.0 as i32 + dc;
        let next_row = current_cell.1 as i32 + dr;
        // If the next cell is a wall, we never should have started
        // toward it — defensively stop and clamp.
        if !cell_open(next_col, next_row) {
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
            && dir_open(current_cell, desired)
        {
            dir = desired;
        } else if !dir_open(current_cell, dir) {
            dir = Dir::None;
            progress = 0.0;
            break;
        }
    }

    // Collect any dot/power-pellet the player is sitting on. The
    // collision condition is "the player's home cell has a dot in
    // it" — we update once per cell-entry, but checking every frame
    // is also fine because the grid clears to 0 after the first hit.
    let (pc, pr) = current_cell;
    unsafe {
        let kind = DOT_GRID[pr as usize][pc as usize];
        if kind != 0 {
            DOT_GRID[pr as usize][pc as usize] = 0;
            DOTS_REMAINING = DOTS_REMAINING.saturating_sub(1);
            SCORE += match kind {
                2 => POWER_PELLET_VALUE,
                _ => DOT_VALUE,
            };
            clear_dot_cell(pc, pr, kind);
            // Chomp-burst sparkles spawn from whichever cell the
            // player just cleared. Power pellets get a bigger
            // burst because they're worth more visually + score-
            // wise.
            let burst = if kind == 2 { PARTICLES_PER_BURST + 3 } else { PARTICLES_PER_BURST };
            spawn_chomp_burst(player_world_centre(), burst);
            if kind == 2 {
                trigger_frightened();
                let _ = audio::voice_trigger(PATCH_PING, NOTE_POWER_PELLET, 110);
            } else {
                let _ = audio::voice_trigger(PATCH_CHOMP, NOTE_CHOMP, 90);
            }
            if DOTS_REMAINING == 0 {
                let _ = audio::voice_trigger(PATCH_PING, NOTE_WIN, 120);
                enter_won();
                return;
            }
        }
    }

    // Commit + push the new world position into the actor.
    unsafe {
        PLAYER_CELL = current_cell;
        PLAYER_DIR = dir;
        PLAYER_PROGRESS = progress;
        if let Some(actor) = PLAYER {
            actor_set_position(actor, player_world_pos(current_cell, dir, progress));
            advance_chomp(actor, dir, dt_ms);
        }
    }

    // ── Ghost tick + collisions ──────────────────────────────────
    update_frightened_timer(dt_ms);
    update_ghosts(dt);
    tick_ghost_visuals(dt_ms);
    check_ghost_collisions();

    // ── Cosmetic systems ─────────────────────────────────────────
    tick_particles(dt_ms);

    // HUD ticks LAST so it anchors to the same final player state
    // the renderer will use in `render`. Earlier in the function
    // `PLAYER_CELL/DIR/PROGRESS` were still mid-update; reading
    // `player_world_centre` there would give a one-frame-old anchor
    // while the camera (set in `render`) reads the new anchor —
    // exactly the 0.7-voxel/frame drift the user was seeing.
    tick_hud();
}

/// Power pellet → blue ghosts for `FRIGHTENED_MS` ms. We just flip
/// the per-ghost `frightened` flag here; the per-frame
/// `tick_ghost_visuals` notices the cached `paint_mat` no longer
/// matches the intended material and repaints.
fn trigger_frightened() {
    unsafe {
        FRIGHTENED_MS_LEFT = FRIGHTENED_MS;
        for i in 0..GHOST_COUNT {
            GHOSTS[i].frightened = true;
        }
    }
}

fn update_frightened_timer(dt_ms: u32) {
    unsafe {
        if FRIGHTENED_MS_LEFT > 0 {
            let prev = FRIGHTENED_MS_LEFT;
            FRIGHTENED_MS_LEFT = FRIGHTENED_MS_LEFT.saturating_sub(dt_ms);
            if prev > 0 && FRIGHTENED_MS_LEFT == 0 {
                // Falling edge — drop all ghosts out of frightened
                // mode. The visual tick handles the actual repaint.
                for i in 0..GHOST_COUNT {
                    GHOSTS[i].frightened = false;
                }
            }
        }
    }
}

fn update_ghosts(dt: f32) {
    let frightened = unsafe { FRIGHTENED_MS_LEFT > 0 };
    let speed = if frightened { GHOST_FRIGHTENED_SPEED_CPS } else { GHOST_SPEED_CPS };
    let player_cell = unsafe { PLAYER_CELL };
    let player_dir = unsafe { PLAYER_DIR };

    // Snapshot Blinky's cell up-front so Inky's target can reference
    // it without aliasing borrows on `GHOSTS`.
    let blinky_cell = unsafe { GHOSTS[0].cell };

    for i in 0..4 {
        let mut g = unsafe { GHOSTS[i] };
        g.progress += speed * dt;
        // Cross zero, one, or multiple cells as needed.
        while g.progress >= 1.0 {
            let (dc, dr) = g.dir.delta();
            let next = (
                (g.cell.0 as i32 + dc) as u32,
                (g.cell.1 as i32 + dr) as u32,
            );
            // We trust that `g.dir` was always picked as an open
            // direction the previous time we crossed a cell — but if
            // somehow it isn't, just stop (defensive).
            if !cell_open(next.0 as i32, next.1 as i32) {
                g.progress = 0.0;
                g.dir = Dir::None;
                break;
            }
            g.cell = next;
            g.progress -= 1.0;
            // Pick the next direction.
            g.dir = pick_ghost_dir(
                &g, player_cell, player_dir, blinky_cell, frightened,
            );
            if matches!(g.dir, Dir::None) {
                // Boxed in — stay put until something opens up. Rare.
                g.progress = 0.0;
                break;
            }
        }
        unsafe {
            GHOSTS[i] = g;
            if let Some(actor) = g.actor {
                actor_set_position(actor, ghost_world_pos(g.cell, g.dir, g.progress));
            }
        }
    }
}

/// Pick a ghost's next direction at a cell boundary. Excludes the
/// 180°-reverse (classic pacman behaviour) and walls. Frightened
/// ghosts wander randomly; otherwise we head toward the ghost's
/// personality target and minimise manhattan distance.
fn pick_ghost_dir(
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
        let pick = (unsafe { rand_u32() } as usize) % n;
        return candidates[pick].0;
    }
    let target = ghost_target(g, player_cell, player_dir, blinky_cell);
    let mut best = candidates[0].0;
    let mut best_d = manhattan_from_step(candidates[0].1, target);
    for &(d, step) in &candidates[1..n] {
        let m = manhattan_from_step(step, target);
        if m < best_d { best_d = m; best = d; }
    }
    best
}

fn ghost_target(
    g: &Ghost,
    player_cell: (u32, u32),
    player_dir: Dir,
    blinky_cell: (u32, u32),
) -> (i32, i32) {
    match g.kind {
        GhostKind::Blinky => (player_cell.0 as i32, player_cell.1 as i32),
        GhostKind::Pinky => {
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

fn encode_step(col: u32, row: u32) -> u32 {
    (col << 16) | row
}

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

// Cart-local xorshift32 — the SDK's host `rand` isn't wired yet, and
// we only need enough randomness to pick a frightened-ghost
// direction. Seeded deterministically; that's fine — repeated runs
// being repeatable is a feature, not a bug.
static mut RNG_STATE: u32 = 0xC0FF_EE17;
unsafe fn rand_u32() -> u32 {
    unsafe {
        let mut x = RNG_STATE;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        RNG_STATE = x;
        x
    }
}

/// `[0.0, 1.0)` float drawn from `rand_u32`.
fn rand_unit() -> f32 {
    (unsafe { rand_u32() } as f32) / (u32::MAX as f32 + 1.0)
}

/// `[-1.0, 1.0)` float drawn from `rand_u32`.
fn rand_signed() -> f32 {
    rand_unit() * 2.0 - 1.0
}

// ── Ghost visuals ─────────────────────────────────────────────────
//
// Per-frame walk over the four ghosts. Advances the wobble phase,
// computes the intended body material (kind colour / frightened blue
// / white-flash near the end of the power pellet window), and
// repaints whenever any of (material, dir, phase) has changed since
// the last paint. The cache keeps the per-frame host traffic flat —
// most frames touch zero ghost voxels.

fn tick_ghost_visuals(dt_ms: u32) {
    let timer = unsafe { FRIGHTENED_MS_LEFT };
    // Globally-timed flash phase so all blue ghosts flash in sync —
    // matches the classic-pacman tell.
    let in_flash_window = timer > 0 && timer < FLASH_WINDOW_MS;
    let flash_white = in_flash_window
        && (timer / FLASH_PERIOD_MS) & 1 == 0;

    for i in 0..GHOST_COUNT {
        let mut g = unsafe { GHOSTS[i] };
        let actor = match g.actor { Some(a) => a, None => continue };

        // Advance the wobble timer; flip phase when it crosses
        // WOBBLE_FRAME_MS. saturating_add keeps us safe across
        // pathologically long frames.
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

        // Eyes keep facing the last non-None direction so a
        // briefly-stopped ghost doesn't snap back to the default
        // east stare.
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

// ── Particles ─────────────────────────────────────────────────────
//
// Pooled actor-based chomp-burst sparkles. `init_particles` runs once
// at boot to spawn the cap of hidden actors; `spawn_chomp_burst`
// hands an arcing trajectory to each free slot; `tick_particles`
// integrates gravity + TTL and hides expired actors back into the
// pool. No collision — particles just fall through the world.

unsafe fn init_particles() {
    prefab_define(
        P_PARTICLE,
        unsafe { &*(&raw const DENSE_PARTICLE) },
        U8Vec3::new(PARTICLE_W as u8, PARTICLE_W as u8, PARTICLE_W as u8),
    );
    let particles = unsafe { &mut *(&raw mut PARTICLES) };
    for p in particles.iter_mut() {
        let id = actor_spawn_from(P_PARTICLE, Orientation::Up)
            .expect("failed to spawn particle");
        actor_set_visible(id, false);
        p.actor = Some(id);
        p.active = false;
    }
}

/// Spawn up to `n` particles arcing outward from `centre`. Picks
/// inactive pool slots first; if the pool is exhausted the surplus
/// is dropped silently rather than reusing a still-airborne slot.
fn spawn_chomp_burst(centre: Vec3, n: usize) {
    let particles = unsafe { &mut *(&raw mut PARTICLES) };
    let mut spawned = 0;
    for p in particles.iter_mut() {
        if spawned >= n { break; }
        if p.active { continue; }
        let actor = match p.actor { Some(a) => a, None => continue };

        // Outward velocity: random in the xz plane, biased upward
        // in y so particles initially launch into the air before
        // gravity arcs them back down. Magnitudes chosen so a
        // ~40-tick lifetime keeps them visible in-screen.
        let vx = rand_signed() * 0.55;
        let vy = 0.65 + rand_unit() * 0.45;
        let vz = rand_signed() * 0.55;

        p.pos = Vec3::new(
            centre.x - PARTICLE_W as f32 * 0.5,
            // Lift the start a bit so particles don't immediately
            // clip into the floor.
            centre.y + 1.5,
            centre.z - PARTICLE_W as f32 * 0.5,
        );
        p.vel = Vec3::new(vx, vy, vz);
        p.ttl_ms = PARTICLE_TTL_MS;
        p.active = true;
        actor_set_visible(actor, true);
        actor_set_position(actor, p.pos);
        spawned += 1;
    }
}

fn tick_particles(dt_ms: u32) {
    let particles = unsafe { &mut *(&raw mut PARTICLES) };
    for p in particles.iter_mut() {
        if !p.active { continue; }
        let actor = match p.actor { Some(a) => a, None => continue };

        // Single Euler step per frame — particles never collide so
        // accuracy doesn't matter, only visual smoothness.
        p.vel.y -= PARTICLE_GRAVITY;
        p.pos.x += p.vel.x;
        p.pos.y += p.vel.y;
        p.pos.z += p.vel.z;

        // Despawn on TTL or when the particle has fallen below the
        // floor (y < 0). Either way the actor goes back to the
        // hidden pool for reuse.
        let ttl_done = p.ttl_ms <= dt_ms;
        if ttl_done || p.pos.y < 0.0 {
            actor_set_visible(actor, false);
            p.active = false;
            continue;
        }
        p.ttl_ms -= dt_ms;
        actor_set_position(actor, p.pos);
    }
}

/// Detect player↔ghost cell overlap. In frightened mode the player
/// eats the ghost (+200, ghost respawns); otherwise the player loses
/// a life and gets reset to their spawn cell.
fn check_ghost_collisions() {
    let pcell = unsafe { PLAYER_CELL };
    let frightened = unsafe { FRIGHTENED_MS_LEFT > 0 };
    for i in 0..4 {
        let g = unsafe { GHOSTS[i] };
        if g.cell != pcell { continue; }
        if frightened {
            unsafe {
                SCORE += GHOST_VALUE;
                GHOSTS[i].cell = g.home;
                GHOSTS[i].progress = 0.0;
                GHOSTS[i].dir = Dir::None;
                // Eaten ghost escapes frightened state immediately —
                // the visual tick will repaint it to its personality
                // colour next frame. Others stay blue until the
                // global timer ends.
                GHOSTS[i].frightened = false;
                if let Some(actor) = g.actor {
                    actor_set_position(actor, ghost_world_pos(g.home, Dir::None, 0.0));
                }
            }
            let _ = audio::voice_trigger(PATCH_PING, NOTE_GHOST_EATEN, 110);
        } else {
            // Death — reset player + all ghosts.
            reset_after_death();
            return;
        }
    }
}

fn reset_after_death() {
    unsafe {
        LIVES = LIVES.saturating_sub(1);
        PLAYER_CELL = (13, 23);
        PLAYER_DIR = Dir::None;
        PLAYER_PROGRESS = 0.0;
        DESIRED_DIR = Dir::None;
        if let Some(actor) = PLAYER {
            actor_set_position(actor, player_world_pos(PLAYER_CELL, Dir::None, 0.0));
        }
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
                actor_set_position(a, ghost_world_pos(home, Dir::None, 0.0));
            }
        }
    }
    // Lives count changed — the per-frame `tick_hud` will toggle the
    // life-icon actor visibility on the next update.
    let _ = audio::voice_trigger(PATCH_PING, NOTE_DEATH, 120);
    if unsafe { LIVES } == 0 {
        enter_lost();
    }
}

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

/// Recolour every wall material slot in one shot — used to flash
/// the whole maze green on win / red on lose. We re-define each
/// material slot at the host level rather than re-emitting voxels,
/// so the change is O(slots) regardless of maze size and avoids
/// disturbing the dot/pip footprints already in the world. The four
/// wall slots all share the requested colour for win/lose since the
/// "everything turns green" tell wants flatness, not detail.
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
    material_define(M_WALL,       Material::pack_color(7, 1), 4,  flags);
    material_define(M_WALL_OUTER, Material::pack_color(7, 2), 6,  flags);
    material_define(M_WALL_CAP,   Material::pack_color(6, 2), 10, flags);
    material_define(M_WALL_PIP,   Material::pack_color(13, 3), 15, MaterialFlags::empty());
}

fn restart_game() {
    unsafe {
        STATE = GameState::Playing;
        LIVES = 3;
        SCORE = 0;
        DOTS_REMAINING = 0;
        FRIGHTENED_MS_LEFT = 0;
        PLAYER_CELL = (13, 23);
        PLAYER_DIR = Dir::None;
        PLAYER_PROGRESS = 0.0;
        DESIRED_DIR = Dir::None;
        // Clear out any leftover dot voxels from the prior round.
        for r in 0..ROWS as usize {
            for c in 0..COLS as usize {
                let k = DOT_GRID[r][c];
                if k != 0 {
                    clear_dot_cell(c as u32, r as u32, k);
                    DOT_GRID[r][c] = 0;
                }
            }
        }
    }
    // Restore every wall slot to its materials.toml defaults — the
    // win/lose flash above flattened all of them to one colour.
    reset_wall_materials();
    // Reseed dots + reset ghost spawn positions.
    paint_maze();
    // Force the HUD score actor to repaint with the freshly-zeroed
    // score on the next tick — without this, `tick_hud`'s skip-if-
    // unchanged path would keep the previous round's number on screen
    // when restarting after a win.
    unsafe { SCORE_LAST_DRAWN = u32::MAX; }
    unsafe {
        for i in 0..GHOST_COUNT {
            let home = GHOSTS[i].home;
            let actor = GHOSTS[i].actor;
            GHOSTS[i].cell = home;
            GHOSTS[i].progress = 0.0;
            GHOSTS[i].dir = Dir::None;
            GHOSTS[i].frightened = false;
            GHOSTS[i].paint_phase = PAINT_PHASE_NEVER;
            if let Some(a) = actor {
                actor_set_position(a, ghost_world_pos(home, Dir::None, 0.0));
            }
        }
        // Reset any lingering particles from the previous round.
        let particles = &mut *(&raw mut PARTICLES);
        for p in particles.iter_mut() {
            if p.active {
                if let Some(a) = p.actor { actor_set_visible(a, false); }
                p.active = false;
            }
        }
        if let Some(actor) = PLAYER {
            actor_set_position(actor, player_world_pos(PLAYER_CELL, Dir::None, 0.0));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    set_follow_camera();
}

/// Position the camera over the player at low altitude with a slight
/// southward tilt so wall sides are visible (depth/3D feel) without
/// the side-asymmetry dominating the frame — that's why the walls
/// are kept short (`WALL_H = 2`).
///
/// Eye sits ~7 cells above the ground; the look target is offset a
/// couple of cells *south* of the eye so the camera tilts back. The
/// player ends up roughly a third of the way up the screen, giving
/// extra forward visibility when moving north.
fn set_follow_camera() {
    const EYE_HEIGHT: f32 = 56.0;
    const TILT_Z: f32 = 16.0;
    let world = player_world_centre();
    camera_set_lookat(
        Vec3::new(world.x, EYE_HEIGHT, world.z + TILT_Z),
        Vec3::new(world.x, 0.0,         world.z),
        Vec3::new(0.0,     0.0,        -1.0),
    );
}

/// World position of the player's geometric centre (the actor's
/// `position` is its local-origin corner; the centre sits a half
/// PLAYER_W in each horizontal axis).
fn player_world_centre() -> Vec3 {
    let cell = unsafe { PLAYER_CELL };
    let dir = unsafe { PLAYER_DIR };
    let progress = unsafe { PLAYER_PROGRESS };
    let origin = player_world_pos(cell, dir, progress);
    Vec3::new(
        origin.x + PLAYER_W as f32 * 0.5,
        origin.y,
        origin.z + PLAYER_W as f32 * 0.5,
    )
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    log("cart panicked");
    loop {}
}

#[allow(dead_code)]
const _UNUSED_DIMS: (u32, u32) = (WORLD_W, WORLD_D);
