//! Action-wheel UX. Primary on a map cell opens the wheel anchored
//! at that cell. While the wheel is open, nav-up / nav-down edges
//! move the highlight through the option list; Confirm or another
//! Primary press commits the pick; Cancel closes the wheel.
//! Confirming WATER queues a drop at the anchor cell; confirming
//! LINE starts a fire-line draft whose first point IS the anchor
//! cell.
//!
//! ## Layout
//!
//! The wheel is rendered across **two stacked 32×32 panels** because
//! the engine caps prefab dimensions at CHUNK_SIZE=32 and the
//! current option list overruns a single panel. The two panels are
//! positioned to read as one continuous menu:
//!
//! - **Top panel**: ACTION header + the first `TOP_OPTION_CAPACITY`
//!   options (5 rows total at LINE_SPACING=6).
//! - **Bottom panel**: every option past that, painted from its row 0.
//!   The panel's screen Y is offset so its row 0 lines up exactly
//!   where row 5 of the top panel would render if the panel were
//!   taller — i.e., no gap between the last top-panel option and the
//!   first bottom-panel option.
//!
//! If all options fit on the top panel, the bottom panel actor is
//! hidden so it doesn't take up screen space.
//!
//! The selection cursor walks the OPTIONS array as a single virtual
//! list; nav-up / nav-down move across panel boundaries naturally.

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

/// Maximum options the top panel can paint *after* the ACTION header.
/// Top panel = 5 rows max at LINE_SPACING=6 (rows 0..4); row 0 is the
/// header, rows 1..4 are option slots. Bumping LINE_SPACING below 6
/// would overlap glyphs (FONT_TINY cell_h=6) so this cap is tight.
const TOP_OPTION_CAPACITY: usize = 4;

const PANEL_W: u32 = 32;
const PANEL_H: u32 = 32;
const PANEL_VOL_BYTES: usize = (PANEL_W * PANEL_H) as usize;
const PANEL_PREFAB_TOP:    PrefabId = PrefabId(70);
const PANEL_PREFAB_BOTTOM: PrefabId = PrefabId(74);

/// Screen-space position of the wheel.
/// The bottom panel's Y is set so its row 0 lands exactly where a
/// hypothetical row 5 of the top panel would (one `LINE_SPACING`
/// step below row 4), giving the two panels a single-menu read.
/// Concretely: top panel row 4 glyph top sits at local Y=6 → screen
/// Y = 28 + (31 - 6) = 53. Next-row screen Y = 53 + 6 = 59. Bottom
/// panel row 0 glyph top is at local Y=30 → screen Y =
/// `WHEEL_SCREEN_Y_BOTTOM + 1` so `WHEEL_SCREEN_Y_BOTTOM = 58`. The
/// resulting 2-pixel overlap with the top panel covers only the
/// top panel's *unpainted* bottom rows (local Y=0..1 outside any
/// glyph extents) so the composite stays clean.
const WHEEL_SCREEN_X:        f32 = 110.0;
const WHEEL_SCREEN_Y_TOP:    f32 = 28.0;
const WHEEL_SCREEN_Y_BOTTOM: f32 = 58.0;
/// Above the sidebar (PANEL_LAYER = 100) so the wheel always paints
/// on top.
const WHEEL_LAYER:    f32 = 200.0;

/// 6 (= FONT_TINY cell height) makes adjacent rows touch with no gap.
const LINE_SPACING: u32 = 6;

static mut PANEL_DENSE_TOP:    [u8; PANEL_VOL_BYTES] = [0; PANEL_VOL_BYTES];
static mut PANEL_DENSE_BOTTOM: [u8; PANEL_VOL_BYTES] = [0; PANEL_VOL_BYTES];

/// How many options end up on top vs bottom for the current OPTIONS
/// array. Always splits top-first up to TOP_OPTION_CAPACITY.
const fn split_counts() -> (usize, usize) {
    let total = OPTIONS.len();
    if total <= TOP_OPTION_CAPACITY {
        (total, 0)
    } else {
        (TOP_OPTION_CAPACITY, total - TOP_OPTION_CAPACITY)
    }
}

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
    /// Per-panel cache key: highlighted option index relative to
    /// that panel (or -1 if the cursor isn't on this panel). Lets
    /// each panel skip repaints when nothing about it changed.
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
        // Idempotent: endless-mode restart calls this again. Just
        // close + clear caches, actors stay.
        if self.actor_top.is_some() {
            self.open = false;
            self.selected = 0;
            self.cache_top = None;
            self.cache_bottom = None;
            if let Some(a) = self.actor_top    { actor_set_visible(a, false); }
            if let Some(a) = self.actor_bottom { actor_set_visible(a, false); }
            return;
        }
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

        // Bottom panel only matters if OPTIONS.len() overruns the
        // top panel; for the current 6-option list it always does,
        // but we conditionally show / hide it in open_at so future
        // shorter lists collapse to a single-panel wheel cleanly.
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
        let (_top_count, bottom_count) = split_counts();
        if let Some(id) = self.actor_top { actor_set_visible(id, true); }
        if let Some(id) = self.actor_bottom {
            actor_set_visible(id, bottom_count > 0);
        }
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
    /// panels share a single virtual option list: indices 0..top_count
    /// render on top (rows 1..top_count+1 below the ACTION header),
    /// indices top_count.. render on bottom (rows 0..).
    pub(crate) fn render(&mut self, _confirm_label: &str) {
        if !self.open { return; }
        let (top_count, bottom_count) = split_counts();

        // Top panel — its cache key is "which option index inside
        // the top panel is highlighted", or -1 when the cursor is on
        // the bottom panel.
        let top_hl: i8 = if (self.selected as usize) < top_count {
            self.selected as i8
        } else {
            -1
        };
        if self.cache_top != Some(top_hl) {
            self.cache_top = Some(top_hl);
            if let Some(actor) = self.actor_top {
                actor_clear(actor);
                paint_line(actor, 0, "ACTION");
                for i in 0..top_count {
                    let (_, label) = OPTIONS[i];
                    let prefix = if top_hl == i as i8 { '>' } else { ' ' };
                    paint_prefixed(actor, (i + 1) as u32, prefix, label);
                }
            }
        }

        // Bottom panel — its cache key is the highlighted option
        // index *relative to the bottom panel* (so the global
        // selection mapping is `top_count + bot_hl`), or -1 when the
        // cursor is on the top panel.
        if bottom_count == 0 {
            // Nothing to render; ensure the cache reflects "no highlight"
            // so a future open with overflow forces a repaint.
            self.cache_bottom = Some(-1);
            return;
        }
        let bot_hl: i8 = if (self.selected as usize) >= top_count {
            (self.selected as usize - top_count) as i8
        } else {
            -1
        };
        if self.cache_bottom != Some(bot_hl) {
            self.cache_bottom = Some(bot_hl);
            if let Some(actor) = self.actor_bottom {
                actor_clear(actor);
                for j in 0..bottom_count {
                    let (_, label) = OPTIONS[top_count + j];
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
