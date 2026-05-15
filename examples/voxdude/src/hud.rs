//! Camera-relative score + lives HUD.
//!
//! The camera is rigidly attached to the player (`set_follow_camera`),
//! so a fixed world-space offset from the player produces a fixed
//! *screen-space* HUD position. Every frame, `tick` recomputes the
//! offsets and pushes them into the score/life actors.
//!
//! Score is rasterised straight into per-digit actor volumes via
//! FONT_ANSI glyph bits, with a 1-voxel dark halo so the digits read
//! against any wall colour. Lives are pre-spawned player-colour cubes
//! that toggle visibility as the count changes.

use voxlconsl_sdk::*;
use voxlconsl_sdk::text;

use crate::{M_HUD_OUTLINE, M_HUD_SCORE, M_PLAYER};
use crate::player;

// ── Geometry ──────────────────────────────────────────────────────

const HUD_Y: f32 = 10.0;

const SCORE_GLYPH_W: u32 = 10;            // FONT_ANSI cell_w
const SCORE_GLYPH_H: u32 = 11;            // FONT_ANSI cell_h
/// Voxel pad around each digit reserved for the dark outline halo
/// painted under the bright digit. 1 voxel border on every side.
const SCORE_PAD:       u32 = 1;
/// Per-digit actor footprint. We split per-digit because the host's
/// prefab chunk size caps at 32, so a single 6-digit actor (62 voxels
/// wide) wouldn't fit — one digit (12 voxels wide) easily does.
/// Digits are placed at integer `SCORE_DIGIT_W` strides so adjacent
/// halos meet exactly without overlapping or gapping.
const SCORE_DIGIT_W:   u32 = SCORE_GLYPH_W + 2 * SCORE_PAD;
const SCORE_DIGIT_D:   u32 = SCORE_GLYPH_H + 2 * SCORE_PAD;
const SCORE_DIGIT_VOL: usize = (SCORE_DIGIT_W * 1 * SCORE_DIGIT_D) as usize;
const SCORE_MAX_DIGITS: usize = 6;

/// North edge of the score row (z offset south of the player). The
/// row's south edge sits `SCORE_DIGIT_D` further south.
const HUD_OFFSET_S: f32 = 14.0;
/// Lives' south edge lines up with the score's south edge so they
/// share a single visual bottom row.
const LIVES_OFFSET_S: f32 = HUD_OFFSET_S + (SCORE_DIGIT_D - LIFE_W) as f32;
/// Left edge of the leftmost score digit, measured from player centre.
const SCORE_LEFT_OFFSET:  f32 = 40.0;
/// Right edge of the rightmost life icon, measured from player centre.
const LIVES_RIGHT_OFFSET: f32 = 40.0;

const LIFE_W:           u32 = 3;
const LIFE_VOL:         usize = (LIFE_W * LIFE_W * LIFE_W) as usize;
const LIFE_SPACING:     f32 = 6.0;
const LIFE_ACTOR_COUNT: usize = 3;

const P_LIFE:        PrefabId = PrefabId(4);
const P_SCORE_DIGIT: PrefabId = PrefabId(5);

// ── State ─────────────────────────────────────────────────────────

static mut SCORE_DIGIT_ACTORS: [Option<ActorId>; SCORE_MAX_DIGITS] =
    [None; SCORE_MAX_DIGITS];
static mut SCORE_LAST_DRAWN:  u32 = u32::MAX;
/// Number of digits currently visible — `tick` reads this to skip
/// repositioning the hidden right-side digits.
static mut SCORE_LAST_DIGITS: u32 = 1;

static mut LIFE_ACTORS: [Option<ActorId>; LIFE_ACTOR_COUNT] = [None; LIFE_ACTOR_COUNT];
static mut DENSE_LIFE:  [u8; LIFE_VOL] = [M_PLAYER; LIFE_VOL];

/// All-air prefab whose dimensions match one score digit's volume.
/// `actor_spawn` would otherwise hand back a fixed 16-cube
/// `OwnedVolume`, so any `actor_set_voxel` past column 15 would
/// silently no-op. Spawning from this prefab makes the fork-on-mutate
/// path inherit the right 12×1×13 size.
static mut DENSE_SCORE_DIGIT: [u8; SCORE_DIGIT_VOL] = [0; SCORE_DIGIT_VOL];

// ── Boot ──────────────────────────────────────────────────────────

/// One-time spawn of the score actors + 3 life-icon actors. The score
/// actors hold per-digit volumes we paint into on score changes; the
/// life actors share a single 3×3×3 player-colour prefab via CoW.
pub(crate) fn init() {
    unsafe {
        prefab_define(
            P_SCORE_DIGIT,
            &*(&raw const DENSE_SCORE_DIGIT),
            U8Vec3::new(SCORE_DIGIT_W as u8, 1, SCORE_DIGIT_D as u8),
        );
        for i in 0..SCORE_MAX_DIGITS {
            let id = actor_spawn_from(P_SCORE_DIGIT, Orientation::Up)
                .expect("score digit actor spawn");
            actor_set_visible(id, false);
            SCORE_DIGIT_ACTORS[i] = Some(id);
        }

        prefab_define(
            P_LIFE,
            &*(&raw const DENSE_LIFE),
            U8Vec3::new(LIFE_W as u8, LIFE_W as u8, LIFE_W as u8),
        );
        for i in 0..LIFE_ACTOR_COUNT {
            let id = actor_spawn_from(P_LIFE, Orientation::Up)
                .expect("life actor spawn");
            LIFE_ACTORS[i] = Some(id);
        }
    }
}

/// Force the score actor to repaint on the next `tick`. Called from
/// `restart_game` so a fresh round doesn't keep the prior round's
/// number on screen.
pub(crate) fn invalidate_score_cache() {
    unsafe { SCORE_LAST_DRAWN = u32::MAX; }
}

// ── Per-frame update ─────────────────────────────────────────────

/// Reposition the score + life actors relative to the player's centre,
/// repainting the score actors if the value changed since last frame.
///
/// Must be called from the END of `update()` (after every player-state
/// mutation has committed) so the HUD and the camera share the same
/// `centre` — that's what keeps the HUD from drifting in the direction
/// of motion by one frame.
pub(crate) fn tick(score: u32, lives: u32) {
    let centre = player::world_centre();

    if score != unsafe { SCORE_LAST_DRAWN } {
        repaint_score(score);
        unsafe { SCORE_LAST_DRAWN = score; }
    }

    // Position each digit actor at a fixed integer offset from the
    // shared score origin. Hidden actors (beyond the digit count) get
    // toggled off so the cleared volume doesn't render as a ghost
    // rectangle.
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
    // rightmost cube sits at `player.x + LIVES_RIGHT_OFFSET`; the row
    // extends leftward by `LIFE_SPACING` per slot. Icons hide from
    // the LEFT as lives are lost so the remaining lives stay anchored
    // to the right edge (classic arcade tell — easier to read than a
    // row that visually drifts).
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

// ── Score rasterisation ──────────────────────────────────────────

/// Rasterise `score` into the score digit actors using FONT_ANSI bits.
/// Walks most-significant first so the leftmost glyph is the highest
/// place; trailing unused digit actors are cleared + hidden.
fn repaint_score(score: u32) {
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
        if i >= len { continue; }
        paint_digit(actor, digits[i]);
    }
}

/// Paint one digit glyph + halo into a single-digit score actor.
///
/// Two-pass paint: outline halo first (8 Moore neighbours of every
/// lit bit, so a continuous 1-voxel dark border surrounds the
/// stroke), then the bright glyph (overwrites the lit bits so they
/// never carry the halo colour).
fn paint_digit(actor: ActorId, ch: u8) {
    let font = &text::FONT_ANSI;
    let cell_w = font.cell_width() as u32;
    let cell_h = font.cell_height() as u32;

    for row in 0..cell_h {
        for col in 0..cell_w {
            if !font.glyph_bit(ch as u32, col as u8, row as u8) { continue; }
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
                    { continue; }
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
            if !font.glyph_bit(ch as u32, col as u8, row as u8) { continue; }
            let x = col + SCORE_PAD;
            let z = row + SCORE_PAD;
            actor_set_voxel(actor, U8Vec3::new(x as u8, 0, z as u8), M_HUD_SCORE);
        }
    }
}
