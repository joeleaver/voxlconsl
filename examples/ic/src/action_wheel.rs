//! Action-wheel UX. Primary on a map cell opens the wheel anchored
//! at that cell. While the wheel is open, nav-up / nav-down edges
//! move the highlight through the option list; Confirm or another
//! Primary press commits the pick; Cancel closes the wheel.
//! Confirming WATER queues a drop at the anchor cell; confirming
//! LINE starts a fire-line draft whose first point IS the anchor
//! cell.
//!
//! The wheel is painted across **two stacked 32×32 panels** (the
//! engine caps prefab dimensions at CHUNK_SIZE=32, and a single panel
//! can't fit ACTION + 5 options at FONT_TINY line spacing). Top
//! panel: ACTION header + WATER / TANKER / RETARD. Bottom panel:
//! LINE / HOTSHOT. The selection cursor moves across both panels as
//! one continuous list — bottom-panel rows just start at index
//! `TOP_OPTIONS`.
//!
//! While the wheel is open, the cursor still moves with the
//! mouse — but the wheel's *anchor* doesn't move. The player can
//! pan the camera, change their mind, or hit Cancel to bail without
//! consequence.

use voxlconsl_sdk::*;
use voxlconsl_sdk::text::FONT_TINY;

use crate::M_HUD_TEXT;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum WheelChoice {
    WaterDrop,
    Tanker,
    Retardant,
    FireLine,
    HotShot,
    Engine,
}

const OPTIONS: [(WheelChoice, &str); 6] = [
    (WheelChoice::WaterDrop, "WATER"),
    (WheelChoice::Tanker,    "TANKER"),
    (WheelChoice::Retardant, "RETARD"),
    (WheelChoice::FireLine,  "LINE"),
    (WheelChoice::HotShot,   "HOTSHO"),
    (WheelChoice::Engine,    "ENGINE"),
];

/// Number of OPTIONS painted onto the top panel (after ACTION header).
/// Indices `[0..TOP_OPTIONS)` render on top; `[TOP_OPTIONS..)` on the
/// bottom panel.
const TOP_OPTIONS: usize = 3;

const PANEL_W: u32 = 32;
const PANEL_H: u32 = 32;
const PANEL_VOL_BYTES: usize = (PANEL_W * PANEL_H) as usize;
const PANEL_PREFAB_TOP:    PrefabId = PrefabId(70);
const PANEL_PREFAB_BOTTOM: PrefabId = PrefabId(74);

/// Screen-space position of the wheel. Two panels stacked vertically:
/// the bottom one sits 32 + GAP below the top.
const WHEEL_SCREEN_X:        f32 = 110.0;
const WHEEL_SCREEN_Y_TOP:    f32 = 28.0;
const WHEEL_PANEL_GAP:       f32 = 4.0;
const WHEEL_SCREEN_Y_BOTTOM: f32 = WHEEL_SCREEN_Y_TOP + PANEL_H as f32 + WHEEL_PANEL_GAP;
/// Above the sidebar (PANEL_LAYER = 100) so the wheel always paints
/// on top.
const WHEEL_LAYER:    f32 = 200.0;

/// 6 (= FONT_TINY cell height) makes adjacent rows touch with no gap.
const LINE_SPACING: u32 = 6;

static mut PANEL_DENSE_TOP:    [u8; PANEL_VOL_BYTES] = [0; PANEL_VOL_BYTES];
static mut PANEL_DENSE_BOTTOM: [u8; PANEL_VOL_BYTES] = [0; PANEL_VOL_BYTES];

pub(crate) struct ActionWheel {
    actor_top:    Option<ActorId>,
    actor_bottom: Option<ActorId>,
    pub open: bool,
    /// 0..OPTIONS.len() — which row is currently highlighted.
    pub selected: u8,
    /// World cell the wheel was opened on. The chosen action
    /// applies to this cell (drop target, or first fire-line
    /// point) regardless of where the cursor has roamed since.
    pub anchor:   UVec3,
    /// Per-panel cache key. None when the highlight is on the
    /// *other* panel (so the row prefix on this panel is always ' ').
    /// Concretely we cache the highlighted row index inside the panel.
    cache_top:    Option<i8>,
    cache_bottom: Option<i8>,
}

impl ActionWheel {
    pub(crate) const fn new() -> Self {
        Self {
            actor_top:    None,
            actor_bottom: None,
            open:  false,
            selected: 0,
            anchor: UVec3 { x: 0, y: 0, z: 0 },
            cache_top:    None,
            cache_bottom: None,
        }
    }

    pub(crate) fn init(&mut self) {
        unsafe {
            prefab_define(
                PANEL_PREFAB_TOP,
                &*(&raw const PANEL_DENSE_TOP),
                U8Vec3::new(PANEL_W as u8, PANEL_H as u8, 1),
            );
            prefab_define(
                PANEL_PREFAB_BOTTOM,
                &*(&raw const PANEL_DENSE_BOTTOM),
                U8Vec3::new(PANEL_W as u8, PANEL_H as u8, 1),
            );
        }
        let top = actor_spawn_from(PANEL_PREFAB_TOP, Orientation::Up)
            .expect("ic action-wheel top spawn");
        actor_set_render_mode(top, ActorRenderMode::Screen);
        actor_set_position(top, Vec3::new(WHEEL_SCREEN_X, WHEEL_SCREEN_Y_TOP, WHEEL_LAYER));
        actor_set_visible(top, false);
        self.actor_top = Some(top);

        let bot = actor_spawn_from(PANEL_PREFAB_BOTTOM, Orientation::Up)
            .expect("ic action-wheel bottom spawn");
        actor_set_render_mode(bot, ActorRenderMode::Screen);
        actor_set_position(bot, Vec3::new(WHEEL_SCREEN_X, WHEEL_SCREEN_Y_BOTTOM, WHEEL_LAYER));
        actor_set_visible(bot, false);
        self.actor_bottom = Some(bot);
    }

    pub(crate) fn open_at(&mut self, cell: UVec3) {
        self.anchor = cell;
        self.selected = 0;
        self.open = true;
        self.cache_top = None;
        self.cache_bottom = None;
        if let Some(id) = self.actor_top    { actor_set_visible(id, true); }
        if let Some(id) = self.actor_bottom { actor_set_visible(id, true); }
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        if let Some(id) = self.actor_top    { actor_set_visible(id, false); }
        if let Some(id) = self.actor_bottom { actor_set_visible(id, false); }
    }

    pub(crate) fn select_prev(&mut self) {
        if self.selected > 0 { self.selected -= 1; }
    }

    pub(crate) fn select_next(&mut self) {
        let last = (OPTIONS.len() as u8).saturating_sub(1);
        if self.selected < last { self.selected += 1; }
    }

    pub(crate) fn current_choice(&self) -> WheelChoice {
        OPTIONS[self.selected as usize % OPTIONS.len()].0
    }

    /// Repaint each panel only when its highlight changed. The two
    /// panels share a single index space; an entry's local row inside
    /// its panel is `selected - TOP_OPTIONS` for the bottom panel and
    /// `selected + 1` (ACTION header sits on row 0) for the top.
    pub(crate) fn render(&mut self, _confirm_label: &str) {
        if !self.open { return; }

        // Top panel highlight: which option index, or -1 if the
        // cursor is on the bottom panel.
        let top_hl: i8 = if (self.selected as usize) < TOP_OPTIONS {
            self.selected as i8
        } else {
            -1
        };
        if self.cache_top != Some(top_hl) {
            self.cache_top = Some(top_hl);
            if let Some(actor) = self.actor_top {
                actor_clear(actor);
                paint_line(actor, 0, "ACTION");
                for i in 0..TOP_OPTIONS {
                    let (_, label) = OPTIONS[i];
                    let prefix = if top_hl == i as i8 { '>' } else { ' ' };
                    paint_prefixed(actor, (i + 1) as u32, prefix, label);
                }
            }
        }

        // Bottom panel highlight: index relative to TOP_OPTIONS, or
        // -1 if cursor is on the top panel.
        let bot_hl: i8 = if (self.selected as usize) >= TOP_OPTIONS {
            (self.selected as usize - TOP_OPTIONS) as i8
        } else {
            -1
        };
        if self.cache_bottom != Some(bot_hl) {
            self.cache_bottom = Some(bot_hl);
            if let Some(actor) = self.actor_bottom {
                actor_clear(actor);
                let bot_count = OPTIONS.len() - TOP_OPTIONS;
                for j in 0..bot_count {
                    let (_, label) = OPTIONS[TOP_OPTIONS + j];
                    let prefix = if bot_hl == j as i8 { '>' } else { ' ' };
                    paint_prefixed(actor, j as u32, prefix, label);
                }
            }
        }
    }
}

/// Paint `"{prefix} {label}"` on row `line_idx` of a 32×32 panel.
fn paint_prefixed(actor: ActorId, line_idx: u32, prefix: char, label: &str) {
    let mut buf = [b' '; 8];
    let mut len = 0;
    buf[len] = prefix as u8; len += 1;
    buf[len] = b' ';        len += 1;
    for &b in label.as_bytes().iter().take(6) {
        if len >= buf.len() { break; }
        buf[len] = b;
        len += 1;
    }
    let s = core::str::from_utf8(&buf[..len]).unwrap_or("");
    paint_line(actor, line_idx, s);
}

/// Paint a string onto a horizontal line of the 32×32 panel actor.
fn paint_line(actor: ActorId, line_idx: u32, text: &str) {
    let cell_w = FONT_TINY.cell_width() as u32;
    let cell_h = FONT_TINY.cell_height() as u32;
    let top_y = (PANEL_H - 1).saturating_sub(1 + line_idx * LINE_SPACING);
    if top_y < cell_h - 1 { return; }

    let mut col_offset = 0u32;
    for ch in text.chars() {
        let cp = ch as u32;
        for row in 0..cell_h {
            for col in 0..cell_w {
                if !FONT_TINY.glyph_bit(cp, col as u8, row as u8) { continue; }
                let x = col_offset + col;
                let y = top_y - row;
                if x >= PANEL_W { continue; }
                actor_set_voxel(actor, U8Vec3::new(x as u8, y as u8, 0), M_HUD_TEXT);
            }
        }
        col_offset += cell_w;
    }
}
