//! On-map badges for queued orders. Each pending order in the
//! CommandQueue gets a small Billboard actor positioned at its
//! target cell, showing its queue position (`1`..`9`) painted in
//! FONT_TINY. Players can tell at a glance where their committed
//! orders are heading and how deep the queue is on each side of
//! the map.
//!
//! - Cyan badges = queued water drops (positioned at the drop cell)
//! - Yellow badges = queued fire lines (positioned at the line's
//!   first waypoint)
//!
//! Beyond position 9, badges aren't drawn — the player can still
//! read the total in the HUD's POOL section.

use voxlconsl_sdk::*;
use voxlconsl_sdk::text::FONT_TINY;

use crate::units::CommandQueue;
use crate::{M_BUCKET_WATER, M_SELECT_MARKER};

pub(crate) const MARKERS_PER_TYPE: usize = 9;
const MARKER_W: u8 = 6;
const MARKER_H: u8 = 8;
const MARKER_VOL_BYTES: usize = (MARKER_W as usize) * (MARKER_H as usize);

const WATER_PREFAB: PrefabId = PrefabId(68);
const LINE_PREFAB:  PrefabId = PrefabId(69);

static mut WATER_DENSE: [u8; MARKER_VOL_BYTES] = [0; MARKER_VOL_BYTES];
static mut LINE_DENSE:  [u8; MARKER_VOL_BYTES] = [0; MARKER_VOL_BYTES];

pub(crate) struct QueueMarkers {
    water_actors: [Option<ActorId>; MARKERS_PER_TYPE],
    line_actors:  [Option<ActorId>; MARKERS_PER_TYPE],
    /// Per-slot cache: (digit_painted, target_x, target_z). When the
    /// queue mutates we walk and update only the slots whose key
    /// changed; idle frames are free.
    water_cache: [Option<(u8, u32, u32)>; MARKERS_PER_TYPE],
    line_cache:  [Option<(u8, u32, u32)>; MARKERS_PER_TYPE],
}

impl QueueMarkers {
    pub(crate) const fn new() -> Self {
        Self {
            water_actors: [None; MARKERS_PER_TYPE],
            line_actors:  [None; MARKERS_PER_TYPE],
            water_cache:  [None; MARKERS_PER_TYPE],
            line_cache:   [None; MARKERS_PER_TYPE],
        }
    }

    pub(crate) fn init(&mut self) {
        // Both badge types share the same blank prefab template;
        // each actor's volume is painted on demand when its slot
        // becomes visible.
        unsafe {
            prefab_define(
                WATER_PREFAB,
                &*(&raw const WATER_DENSE),
                U8Vec3::new(MARKER_W, MARKER_H, 1),
            );
            prefab_define(
                LINE_PREFAB,
                &*(&raw const LINE_DENSE),
                U8Vec3::new(MARKER_W, MARKER_H, 1),
            );
        }
        for slot in &mut self.water_actors {
            let id = actor_spawn_from(WATER_PREFAB, Orientation::Up)
                .expect("queue water marker spawn");
            actor_set_render_mode(id, ActorRenderMode::Billboard);
            actor_set_visible(id, false);
            *slot = Some(id);
        }
        for slot in &mut self.line_actors {
            let id = actor_spawn_from(LINE_PREFAB, Orientation::Up)
                .expect("queue line marker spawn");
            actor_set_render_mode(id, ActorRenderMode::Billboard);
            actor_set_visible(id, false);
            *slot = Some(id);
        }
    }

    /// Walk the queue and sync the marker pool. Cached so unchanged
    /// slots cost only the cache compare.
    pub(crate) fn update(&mut self, queue: &CommandQueue) {
        let n_water = (queue.pending_water() as usize).min(MARKERS_PER_TYPE);
        for i in 0..MARKERS_PER_TYPE {
            let actor = match self.water_actors[i] { Some(a) => a, None => continue };
            if i >= n_water {
                if self.water_cache[i].is_some() {
                    actor_set_visible(actor, false);
                    self.water_cache[i] = None;
                }
                continue;
            }
            let target = match queue.water_at(i) {
                Some(t) => t,
                None    => continue,
            };
            let digit = (i + 1).min(9) as u8;
            let key = (digit, target.x, target.z);
            if self.water_cache[i] != Some(key) {
                actor_clear(actor);
                paint_badge(actor, digit, M_BUCKET_WATER);
                self.water_cache[i] = Some(key);
            }
            actor_set_position(
                actor,
                Vec3::new(target.x as f32 + 0.5, target.y as f32 + 4.0, target.z as f32 + 0.5),
            );
            actor_set_visible(actor, true);
        }

        let n_line = (queue.pending_lines() as usize).min(MARKERS_PER_TYPE);
        for i in 0..MARKERS_PER_TYPE {
            let actor = match self.line_actors[i] { Some(a) => a, None => continue };
            if i >= n_line {
                if self.line_cache[i].is_some() {
                    actor_set_visible(actor, false);
                    self.line_cache[i] = None;
                }
                continue;
            }
            let target = match queue.line_head_at(i) {
                Some(t) => t,
                None    => continue,
            };
            let digit = (i + 1).min(9) as u8;
            let key = (digit, target.x, target.z);
            if self.line_cache[i] != Some(key) {
                actor_clear(actor);
                paint_badge(actor, digit, M_SELECT_MARKER);
                self.line_cache[i] = Some(key);
            }
            actor_set_position(
                actor,
                Vec3::new(target.x as f32 + 0.5, target.y as f32 + 4.0, target.z as f32 + 0.5),
            );
            actor_set_visible(actor, true);
        }
    }
}

/// Paint a single-digit badge into a Billboard actor's volume. The
/// digit is placed at local (1, 1, 0) so it has a 1-pixel transparent
/// margin against the edge of the badge.
fn paint_badge(actor: ActorId, digit: u8, color: u8) {
    let cp = (b'0' + digit) as u32;
    let cell_w = FONT_TINY.cell_width() as u8;
    let cell_h = FONT_TINY.cell_height() as u8;
    // Position the glyph centred in the 6×8 actor volume. The font
    // cell is 4×6, so a 1-px margin on each side puts the glyph at
    // (1..5, 1..7).
    let x_off: u8 = 1;
    let y_top: u8 = MARKER_H - 1 - 1;     // top row of glyph, leaving 1-px top margin
    for row in 0..cell_h {
        for col in 0..cell_w {
            if !FONT_TINY.glyph_bit(cp, col, row) { continue; }
            let x = x_off + col;
            let y = y_top - row;
            actor_set_voxel(actor, U8Vec3::new(x, y, 0), color);
        }
    }
}
