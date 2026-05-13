//! On-map badges for queued AND in-flight orders. Each order gets a
//! small Billboard actor positioned at its target cell, showing its
//! position (`1`..`9`) painted in FONT_TINY. Players can tell at a
//! glance where their committed orders are heading and how deep the
//! pipeline is on each side of the map.
//!
//! - **Cyan**: heli water drops (positioned at the drop cell).
//! - **Yellow (select-marker)**: firetruck fire-line orders
//!   (positioned at the line's first waypoint).
//! - **Cyan / pink** (tanker stripe): pending tanker sorties.
//! - **Yellow (hotshot stripe)**: hot-shot squad orders (positioned
//!   at the squad's line anchor — same cell while the drop plane is
//!   in the air, while the chutes are falling, and while the crews
//!   are on the ground).
//! - **Red (engine body)**: fire-engine park orders.
//!
//! Badges include **active** orders (a unit is already executing
//! them) followed by **queued** orders (waiting for an idle unit).
//! An active order's badge stays visible until the unit completes,
//! so a single dispatch on an idle pool still shows a badge until
//! the drop actually lands.
//!
//! Beyond position 9, badges aren't drawn — the player can still
//! read the total in the HUD's POOL section.

use voxlconsl_sdk::*;
use voxlconsl_sdk::text::FONT_TINY;

use crate::terrain::terrain_height;
use crate::units::{Roster, TankerKind};
use crate::{
    M_BUCKET_WATER, M_ENGINE_BODY, M_HOTSHOT_STRIPE, M_SELECT_MARKER,
    M_TANKER_RETARDANT_STRIPE, M_TANKER_WATER_STRIPE,
};

pub(crate) const MARKERS_PER_TYPE: usize = 9;
const MARKER_W: u8 = 6;
const MARKER_H: u8 = 8;
const MARKER_VOL_BYTES: usize = (MARKER_W as usize) * (MARKER_H as usize);

const WATER_PREFAB:        PrefabId = PrefabId(68);
const LINE_PREFAB:         PrefabId = PrefabId(69);
const TANKER_REQ_PREFAB:   PrefabId = PrefabId(71);
const HOTSHOT_PREFAB:      PrefabId = PrefabId(76);
const ENGINE_PREFAB:       PrefabId = PrefabId(77);

static mut WATER_DENSE:      [u8; MARKER_VOL_BYTES] = [0; MARKER_VOL_BYTES];
static mut LINE_DENSE:       [u8; MARKER_VOL_BYTES] = [0; MARKER_VOL_BYTES];
static mut TANKER_REQ_DENSE: [u8; MARKER_VOL_BYTES] = [0; MARKER_VOL_BYTES];
static mut HOTSHOT_DENSE:    [u8; MARKER_VOL_BYTES] = [0; MARKER_VOL_BYTES];
static mut ENGINE_DENSE:     [u8; MARKER_VOL_BYTES] = [0; MARKER_VOL_BYTES];

pub(crate) struct QueueMarkers {
    water_actors:        [Option<ActorId>; MARKERS_PER_TYPE],
    line_actors:         [Option<ActorId>; MARKERS_PER_TYPE],
    /// Badges over each queued tanker request (water or retardant).
    /// Cache stores the colour too so a position change AND a kind
    /// change (rare — when a request slot recycles between
    /// different-kind requests) both trigger a repaint.
    tanker_req_actors:   [Option<ActorId>; MARKERS_PER_TYPE],
    hotshot_actors:      [Option<ActorId>; MARKERS_PER_TYPE],
    engine_actors:       [Option<ActorId>; MARKERS_PER_TYPE],
    water_cache:      [Option<(u8, u32, u32)>; MARKERS_PER_TYPE],
    line_cache:       [Option<(u8, u32, u32)>; MARKERS_PER_TYPE],
    tanker_req_cache: [Option<(u8, u32, u32, u8)>; MARKERS_PER_TYPE],
    hotshot_cache:    [Option<(u8, u32, u32)>; MARKERS_PER_TYPE],
    engine_cache:     [Option<(u8, u32, u32)>; MARKERS_PER_TYPE],
}

impl QueueMarkers {
    pub(crate) const fn new() -> Self {
        Self {
            water_actors:      [None; MARKERS_PER_TYPE],
            line_actors:       [None; MARKERS_PER_TYPE],
            tanker_req_actors: [None; MARKERS_PER_TYPE],
            hotshot_actors:    [None; MARKERS_PER_TYPE],
            engine_actors:     [None; MARKERS_PER_TYPE],
            water_cache:      [None; MARKERS_PER_TYPE],
            line_cache:       [None; MARKERS_PER_TYPE],
            tanker_req_cache: [None; MARKERS_PER_TYPE],
            hotshot_cache:    [None; MARKERS_PER_TYPE],
            engine_cache:     [None; MARKERS_PER_TYPE],
        }
    }

    pub(crate) fn init(&mut self) {
        // All badge types share the same blank prefab template;
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
            prefab_define(
                TANKER_REQ_PREFAB,
                &*(&raw const TANKER_REQ_DENSE),
                U8Vec3::new(MARKER_W, MARKER_H, 1),
            );
            prefab_define(
                HOTSHOT_PREFAB,
                &*(&raw const HOTSHOT_DENSE),
                U8Vec3::new(MARKER_W, MARKER_H, 1),
            );
            prefab_define(
                ENGINE_PREFAB,
                &*(&raw const ENGINE_DENSE),
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
        for slot in &mut self.tanker_req_actors {
            let id = actor_spawn_from(TANKER_REQ_PREFAB, Orientation::Up)
                .expect("queue tanker-req marker spawn");
            actor_set_render_mode(id, ActorRenderMode::Billboard);
            actor_set_visible(id, false);
            *slot = Some(id);
        }
        for slot in &mut self.hotshot_actors {
            let id = actor_spawn_from(HOTSHOT_PREFAB, Orientation::Up)
                .expect("queue hotshot marker spawn");
            actor_set_render_mode(id, ActorRenderMode::Billboard);
            actor_set_visible(id, false);
            *slot = Some(id);
        }
        for slot in &mut self.engine_actors {
            let id = actor_spawn_from(ENGINE_PREFAB, Orientation::Up)
                .expect("queue engine marker spawn");
            actor_set_render_mode(id, ActorRenderMode::Billboard);
            actor_set_visible(id, false);
            *slot = Some(id);
        }
    }

    /// Walk active units and the queue, syncing the marker pool to
    /// the merged list. Active orders take the low slots (so the
    /// soonest-to-finish badges are 1, 2, …), queued orders fill in
    /// behind. Cached so unchanged slots cost only the cache compare.
    pub(crate) fn update(&mut self, roster: &Roster) {
        // ── Water drops ──
        let mut idx = 0usize;
        for slot in roster.helis.iter() {
            if idx >= MARKERS_PER_TYPE { break; }
            let h = match slot { Some(h) => h, None => continue };
            let (x, z) = match h.active_drop_target() { Some(c) => c, None => continue };
            let y = terrain_height(x, z);
            refresh_slot(
                self.water_actors[idx], &mut self.water_cache[idx],
                idx, UVec3::new(x, y, z), M_BUCKET_WATER,
            );
            idx += 1;
        }
        for i in 0..(roster.queue.pending_water() as usize) {
            if idx >= MARKERS_PER_TYPE { break; }
            let target = match roster.queue.water_at(i) { Some(t) => t, None => continue };
            refresh_slot(
                self.water_actors[idx], &mut self.water_cache[idx],
                idx, target, M_BUCKET_WATER,
            );
            idx += 1;
        }
        for i in idx..MARKERS_PER_TYPE {
            hide_slot(self.water_actors[i], &mut self.water_cache[i]);
        }

        // ── Fire lines ──
        let mut idx = 0usize;
        for slot in roster.crews.iter() {
            if idx >= MARKERS_PER_TYPE { break; }
            let c = match slot { Some(c) => c, None => continue };
            let (x, z) = match c.active_line_head() { Some(c) => c, None => continue };
            let y = terrain_height(x, z);
            refresh_slot(
                self.line_actors[idx], &mut self.line_cache[idx],
                idx, UVec3::new(x, y, z), M_SELECT_MARKER,
            );
            idx += 1;
        }
        for i in 0..(roster.queue.pending_lines() as usize) {
            if idx >= MARKERS_PER_TYPE { break; }
            let target = match roster.queue.line_head_at(i) { Some(t) => t, None => continue };
            refresh_slot(
                self.line_actors[idx], &mut self.line_cache[idx],
                idx, target, M_SELECT_MARKER,
            );
            idx += 1;
        }
        for i in idx..MARKERS_PER_TYPE {
            hide_slot(self.line_actors[i], &mut self.line_cache[i]);
        }

        // ── Tanker requests (shared between water + retardant) ──
        // Each pending sortie gets a badge floating over the strip's
        // midpoint while it waits for a tanker slot. Cyan for water,
        // salmon-pink for retardant; queue position is the digit.
        let mut idx = 0usize;
        for slot in roster.tanker_requests.iter() {
            if idx >= MARKERS_PER_TYPE { break; }
            let req = match slot { Some(r) => r, None => continue };
            let color = match req.kind {
                TankerKind::Water     => M_TANKER_WATER_STRIPE,
                TankerKind::Retardant => M_TANKER_RETARDANT_STRIPE,
            };
            refresh_tanker_slot(
                self.tanker_req_actors[idx], &mut self.tanker_req_cache[idx],
                idx, req.badge_cell(), color,
            );
            idx += 1;
        }
        for i in idx..MARKERS_PER_TYPE {
            hide_tanker_slot(self.tanker_req_actors[i], &mut self.tanker_req_cache[i]);
        }

        // ── Hot-shot squads ──
        // A single squad order spans a drop plane + ≤4 parachutes +
        // ≤4 crews on the ground, all sharing the same path[0]
        // anchor. We dedupe by (x, z) so the player sees ONE badge
        // per squad regardless of phase. Active squads first, then
        // queued orders.
        let mut idx = 0usize;
        let mut seen: [(u32, u32); MARKERS_PER_TYPE] = [(u32::MAX, u32::MAX); MARKERS_PER_TYPE];
        for slot in roster.drop_planes.iter() {
            if idx >= MARKERS_PER_TYPE { break; }
            let plane = match slot { Some(p) => p, None => continue };
            let Some(anchor) = plane.path[0] else { continue };
            if mark_seen(&mut seen, idx, anchor) {
                refresh_slot(
                    self.hotshot_actors[idx], &mut self.hotshot_cache[idx],
                    idx, UVec3::new(anchor.0, terrain_height(anchor.0, anchor.1), anchor.1),
                    M_HOTSHOT_STRIPE,
                );
                idx += 1;
            }
        }
        for slot in roster.parachutes.iter() {
            if idx >= MARKERS_PER_TYPE { break; }
            let chute = match slot { Some(p) => p, None => continue };
            let Some(anchor) = chute.path[0] else { continue };
            if mark_seen(&mut seen, idx, anchor) {
                refresh_slot(
                    self.hotshot_actors[idx], &mut self.hotshot_cache[idx],
                    idx, UVec3::new(anchor.0, terrain_height(anchor.0, anchor.1), anchor.1),
                    M_HOTSHOT_STRIPE,
                );
                idx += 1;
            }
        }
        for slot in roster.hotshots.iter() {
            if idx >= MARKERS_PER_TYPE { break; }
            let hs = match slot { Some(h) => h, None => continue };
            let Some(anchor) = hs.active_line_head() else { continue };
            if mark_seen(&mut seen, idx, anchor) {
                refresh_slot(
                    self.hotshot_actors[idx], &mut self.hotshot_cache[idx],
                    idx, UVec3::new(anchor.0, terrain_height(anchor.0, anchor.1), anchor.1),
                    M_HOTSHOT_STRIPE,
                );
                idx += 1;
            }
        }
        for i in 0..(roster.queue.pending_hotshots() as usize) {
            if idx >= MARKERS_PER_TYPE { break; }
            let target = match roster.queue.hotshot_head_at(i) { Some(t) => t, None => continue };
            refresh_slot(
                self.hotshot_actors[idx], &mut self.hotshot_cache[idx],
                idx, target, M_HOTSHOT_STRIPE,
            );
            idx += 1;
        }
        for i in idx..MARKERS_PER_TYPE {
            hide_slot(self.hotshot_actors[i], &mut self.hotshot_cache[i]);
        }

        // ── Fire engines ──
        // One badge per active or queued engine order. Engines have
        // at most one order each, so no dedupe required.
        let mut idx = 0usize;
        for slot in roster.engines.iter() {
            if idx >= MARKERS_PER_TYPE { break; }
            let e = match slot { Some(e) => e, None => continue };
            let (x, z) = match e.active_target() { Some(c) => c, None => continue };
            let y = terrain_height(x, z);
            refresh_slot(
                self.engine_actors[idx], &mut self.engine_cache[idx],
                idx, UVec3::new(x, y, z), M_ENGINE_BODY,
            );
            idx += 1;
        }
        for i in 0..(roster.queue.pending_engines() as usize) {
            if idx >= MARKERS_PER_TYPE { break; }
            let target = match roster.queue.engine_at(i) { Some(t) => t, None => continue };
            refresh_slot(
                self.engine_actors[idx], &mut self.engine_cache[idx],
                idx, target, M_ENGINE_BODY,
            );
            idx += 1;
        }
        for i in idx..MARKERS_PER_TYPE {
            hide_slot(self.engine_actors[i], &mut self.engine_cache[i]);
        }
    }
}

/// Push `cell` into the first `next` slots of `seen` if it isn't
/// already present. Returns `true` iff `cell` was newly added.
fn mark_seen(
    seen: &mut [(u32, u32); MARKERS_PER_TYPE],
    next: usize,
    cell: (u32, u32),
) -> bool {
    for i in 0..next {
        if seen[i] == cell { return false; }
    }
    if next < MARKERS_PER_TYPE {
        seen[next] = cell;
    }
    true
}

/// Same shape as `refresh_slot` but with a 4-tuple cache key that
/// includes the colour, so a request slot that recycles from a
/// water-tanker badge to a retardant-tanker badge (same position,
/// same XZ, different colour) still triggers a repaint.
fn refresh_tanker_slot(
    actor: Option<ActorId>,
    cache: &mut Option<(u8, u32, u32, u8)>,
    slot: usize,
    target: UVec3,
    color: u8,
) {
    let actor = match actor { Some(a) => a, None => return };
    let digit = (slot + 1).min(9) as u8;
    let key = (digit, target.x, target.z, color);
    if *cache != Some(key) {
        actor_clear(actor);
        paint_badge(actor, digit, color);
        *cache = Some(key);
    }
    actor_set_position(
        actor,
        Vec3::new(target.x as f32 + 0.5, target.y as f32 + 4.0, target.z as f32 + 0.5),
    );
    actor_set_visible(actor, true);
}

fn hide_tanker_slot(actor: Option<ActorId>, cache: &mut Option<(u8, u32, u32, u8)>) {
    let actor = match actor { Some(a) => a, None => return };
    if cache.is_some() {
        actor_set_visible(actor, false);
        *cache = None;
    }
}

/// Paint + position + show a single badge slot. The cache compares
/// the (digit, target) tuple so repaints only happen on real changes.
fn refresh_slot(
    actor: Option<ActorId>,
    cache: &mut Option<(u8, u32, u32)>,
    slot: usize,
    target: UVec3,
    color: u8,
) {
    let actor = match actor { Some(a) => a, None => return };
    let digit = (slot + 1).min(9) as u8;
    let key = (digit, target.x, target.z);
    if *cache != Some(key) {
        actor_clear(actor);
        paint_badge(actor, digit, color);
        *cache = Some(key);
    }
    actor_set_position(
        actor,
        Vec3::new(target.x as f32 + 0.5, target.y as f32 + 4.0, target.z as f32 + 0.5),
    );
    actor_set_visible(actor, true);
}

fn hide_slot(actor: Option<ActorId>, cache: &mut Option<(u8, u32, u32)>) {
    let actor = match actor { Some(a) => a, None => return };
    if cache.is_some() {
        actor_set_visible(actor, false);
        *cache = None;
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
