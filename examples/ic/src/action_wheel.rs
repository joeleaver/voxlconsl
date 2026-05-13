//! Action-wheel UX. Primary on a map cell opens the wheel anchored
//! at that cell. While the wheel is open, nav-up / nav-down edges
//! move the highlight through the option list; Confirm or another
//! Primary press commits the pick; Cancel closes the wheel.
//! Confirming WATER queues a drop at the anchor cell; confirming
//! LINE starts a fire-line draft whose first point IS the anchor
//! cell.
//!
//! While the wheel is open, the cursor still moves with the
//! mouse — but the wheel's *anchor* doesn't move. The player can
//! pan the camera, change their mind, or hit Esc to bail without
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
}

const OPTIONS: [(WheelChoice, &str); 4] = [
    (WheelChoice::WaterDrop, "WATER"),
    (WheelChoice::Tanker,    "TANKER"),
    (WheelChoice::Retardant, "RETARD"),
    (WheelChoice::FireLine,  "LINE"),
];

const PANEL_W: u32 = 32;
/// 32 — engine caps prefab dimensions at CHUNK_SIZE = 32, so the
/// wheel has to live inside a single 32³ volume. Anything taller is
/// silently rejected by `prefabs::define`.
const PANEL_H: u32 = 32;
const PANEL_VOL_BYTES: usize = (PANEL_W * PANEL_H) as usize;
const PANEL_PREFAB: PrefabId = PrefabId(70);

/// Screen-space position of the wheel's upper-left corner. Sits to
/// the right of the sidebar in the empty-world strip so it doesn't
/// occlude anything meaningful when open.
const WHEEL_SCREEN_X: f32 = 110.0;
const WHEEL_SCREEN_Y: f32 = 52.0;
/// Above the sidebar (PANEL_LAYER = 100) so the wheel always paints
/// on top.
const WHEEL_LAYER:    f32 = 200.0;

/// 6 (= FONT_TINY cell height) makes adjacent rows touch with no gap.
/// Tight, but lets us fit ACTION + 4 options inside the 32-px panel.
/// Drop the hint row entirely — the cart's HUD HELP sidebar shows
/// the same confirm-key reminder.
const LINE_SPACING: u32 = 6;

static mut PANEL_DENSE: [u8; PANEL_VOL_BYTES] = [0; PANEL_VOL_BYTES];

pub(crate) struct ActionWheel {
    actor:    Option<ActorId>,
    pub open: bool,
    /// 0..OPTIONS.len() — which row is currently highlighted.
    pub selected: u8,
    /// World cell the wheel was opened on. The chosen action
    /// applies to this cell (drop target, or first fire-line
    /// point) regardless of where the cursor has roamed since.
    pub anchor:   UVec3,
    /// Cache so the wheel only repaints when the highlighted option
    /// or the host-provided confirm-binding label changes.
    cache: Option<u16>,
}

impl ActionWheel {
    pub(crate) const fn new() -> Self {
        Self {
            actor: None,
            open:  false,
            selected: 0,
            anchor: UVec3 { x: 0, y: 0, z: 0 },
            cache: None,
        }
    }

    pub(crate) fn init(&mut self) {
        unsafe {
            prefab_define(
                PANEL_PREFAB,
                &*(&raw const PANEL_DENSE),
                U8Vec3::new(PANEL_W as u8, PANEL_H as u8, 1),
            );
        }
        let id = actor_spawn_from(PANEL_PREFAB, Orientation::Up)
            .expect("ic action-wheel spawn");
        actor_set_render_mode(id, ActorRenderMode::Screen);
        actor_set_position(id, Vec3::new(WHEEL_SCREEN_X, WHEEL_SCREEN_Y, WHEEL_LAYER));
        actor_set_visible(id, false);
        self.actor = Some(id);
    }

    pub(crate) fn open_at(&mut self, cell: UVec3) {
        self.anchor = cell;
        self.selected = 0;
        self.open = true;
        self.cache = None;
        if let Some(id) = self.actor { actor_set_visible(id, true); }
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        if let Some(id) = self.actor { actor_set_visible(id, false); }
    }

    /// Move highlight one step toward the top of the list. Clamps
    /// at index 0 so holding the nav-up direction doesn't wrap past
    /// the player.
    pub(crate) fn select_prev(&mut self) {
        if self.selected > 0 { self.selected -= 1; }
    }

    /// Move highlight one step toward the bottom of the list,
    /// clamped at the last option.
    pub(crate) fn select_next(&mut self) {
        let last = (OPTIONS.len() as u8).saturating_sub(1);
        if self.selected < last { self.selected += 1; }
    }

    pub(crate) fn current_choice(&self) -> WheelChoice {
        OPTIONS[self.selected as usize % OPTIONS.len()].0
    }

    /// Repaint when the highlighted option changes. The bottom hint
    /// was dropped to fit ACTION + 4 options inside the 32-px panel —
    /// the cart's HUD HELP sidebar shows the same confirm/cancel keys.
    /// `_confirm_label` is left in the signature so the cart's frame
    /// loop can keep handing the label down once we have room again.
    pub(crate) fn render(&mut self, _confirm_label: &str) {
        if !self.open { return; }
        if self.cache == Some(self.selected as u16) { return; }
        self.cache = Some(self.selected as u16);
        let actor = match self.actor { Some(a) => a, None => return };
        actor_clear(actor);

        paint_line(actor, 0, "ACTION");
        for (i, (_choice, label)) in OPTIONS.iter().enumerate() {
            let prefix = if i == self.selected as usize { '>' } else { ' ' };
            let mut buf = [b' '; 8];
            let mut len = 0;
            buf[len] = prefix as u8; len += 1;
            buf[len] = b' '; len += 1;
            for &b in label.as_bytes().iter().take(6) {
                if len >= buf.len() { break; }
                buf[len] = b;
                len += 1;
            }
            let s = core::str::from_utf8(&buf[..len]).unwrap_or("");
            paint_line(actor, (i + 1) as u32, s);
        }
    }
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
