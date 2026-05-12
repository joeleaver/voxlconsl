//! Phase-1 HUD: one `Screen`-mode actor anchored to the top-left of
//! the framebuffer, showing TIME / TOWN / FIRE / UNIT in FONT_TINY.
//!
//! The actor's volume is `64 × 36 × 1` (panel rect on the screen),
//! defined via a prefab so the spawn inherits the right size — a
//! plain `actor_spawn` would hand back a 16³ volume that can't fit
//! the panel. Each frame the cart re-rasterises the four lines into
//! the actor's voxel grid (only when values change — caching the
//! same key as before).
//!
//! Coordinate convention inside the actor (matches the engine's
//! Screen-mode blit):
//!
//! - local `+X` → screen-right
//! - local `+Y` → screen-up (so glyph row 0, the top of a letter,
//!   maps to the HIGHEST local Y for its line)
//! - local `+Z` → unused for compositing; we paint everything at z=0

use voxlconsl_sdk::*;
use voxlconsl_sdk::text::{Font, FONT_TINY};

use crate::{M_HUD_TEXT};

// ── Panel size + layout ──────────────────────────────────────────

// Prefabs cap each axis at 32 (one SVO chunk), so the panel is 32².
// Single-char labels keep the lines under the 32-pixel width budget:
//   "T 0:00"  = 24 px       "F 999"   = 20 px
//   "C 6/6"   = 20 px       "U HELI"  = 24 px
const PANEL_W: u32 = 32;
const PANEL_H: u32 = 32;
const PANEL_VOL_BYTES: usize = (PANEL_W * PANEL_H * 1) as usize;

const PANEL_PREFAB: PrefabId = PrefabId(64);

/// Screen-space pixel position of the panel's upper-left corner.
const PANEL_SCREEN_X: f32 = 3.0;
const PANEL_SCREEN_Y: f32 = 3.0;
/// Layer (drawn after lower z; lower z paints first).
const PANEL_LAYER:    f32 = 100.0;

/// Line spacing in voxel rows (cell_h = 6 + 1 gap = 7).
const LINE_SPACING: u32 = 7;

const SIDEBAR_LINE_MAX: usize = 8;

// Backing buffer for the prefab. All-air; the actor's owned-volume
// fork happens on the first `actor_set_voxel` call, after which we
// paint into it freely.
static mut PANEL_DENSE: [u8; PANEL_VOL_BYTES] = [0; PANEL_VOL_BYTES];

// ── State ─────────────────────────────────────────────────────────

pub(crate) struct Hud {
    actor: Option<ActorId>,
    cache: Option<(u32, u32, u32, [u8; 4])>,    // t_sec, alive, fire, label[4]
}

impl Hud {
    pub(crate) const fn new() -> Self {
        Self { actor: None, cache: None }
    }

    /// One-time setup. Registers the panel prefab and spawns the
    /// sidebar actor in Screen render mode.
    pub(crate) fn init(&mut self) {
        unsafe {
            prefab_define(
                PANEL_PREFAB,
                &*(&raw const PANEL_DENSE),
                U8Vec3::new(PANEL_W as u8, PANEL_H as u8, 1),
            );
        }
        let id = actor_spawn_from(PANEL_PREFAB, Orientation::Up)
            .expect("ic HUD actor spawn");
        actor_set_render_mode(id, ActorRenderMode::Screen);
        actor_set_position(id, Vec3::new(PANEL_SCREEN_X, PANEL_SCREEN_Y, PANEL_LAYER));
        self.actor = Some(id);
    }

    /// Re-rasterise the panel when any displayed value has changed.
    pub(crate) fn paint(
        &mut self,
        t_ms: u32,
        survival: u32,
        unit_label: &str,
    ) {
        let actor = match self.actor {
            Some(a) => a,
            None => return,
        };

        let alive = survival.count_ones();
        let fire_sites = crate::fire_sites_sampled();
        let t_sec = t_ms / 1000;
        let mut label_key = [0u8; 4];
        let lb = unit_label.as_bytes();
        for i in 0..4.min(lb.len()) { label_key[i] = lb[i]; }
        let key = (t_sec, alive, fire_sites, label_key);
        if self.cache == Some(key) { return; }
        self.cache = Some(key);

        actor_clear(actor);

        let mut buf = [b' '; SIDEBAR_LINE_MAX];

        let s = format_time(&mut buf, t_ms);
        paint_line(actor, &FONT_TINY, 0, 0, M_HUD_TEXT, s);
        let s = format_town(&mut buf, alive);
        paint_line(actor, &FONT_TINY, 1, 0, M_HUD_TEXT, s);
        let s = format_fire(&mut buf, fire_sites);
        paint_line(actor, &FONT_TINY, 2, 0, M_HUD_TEXT, s);
        let s = format_unit(&mut buf, unit_label);
        paint_line(actor, &FONT_TINY, 3, 0, M_HUD_TEXT, s);
    }
}

/// Paint `text` onto a horizontal line of the sidebar actor. Line 0
/// is the top of the panel. `start_col` shifts the text right inside
/// the line in pixel columns. Glyphs that hang off the right edge
/// are silently clipped.
fn paint_line(
    actor: ActorId,
    font: &Font<'_>,
    line_idx: u32,
    start_col: u32,
    color: u8,
    text: &str,
) {
    let cell_w = font.cell_width() as u32;
    let cell_h = font.cell_height() as u32;
    // Top row of this line in actor-local Y. Line 0 starts at the
    // panel's top, with a 1-pixel top margin so the highest stroke
    // doesn't run against the edge.
    let line_top_y = (PANEL_H - 1).saturating_sub(1 + line_idx * LINE_SPACING);
    if line_top_y < cell_h - 1 { return; }

    let mut col_offset = 0u32;
    for ch in text.chars() {
        let cp = ch as u32;
        for row in 0..cell_h {
            for col in 0..cell_w {
                if !font.glyph_bit(cp, col as u8, row as u8) { continue; }
                let x = start_col + col_offset + col;
                let y = line_top_y - row;
                if x >= PANEL_W { continue; }
                actor_set_voxel(actor, U8Vec3::new(x as u8, y as u8, 0), color);
            }
        }
        col_offset += cell_w;
    }
}

// ── Sidebar formatters (no_std + no_alloc) ────────────────────────

fn format_time<'a>(buf: &'a mut [u8], t_ms: u32) -> &'a str {
    let sec = t_ms / 1000;
    let m = (sec / 60) % 10;
    let s = sec % 60;
    buf[..2].copy_from_slice(b"T ");
    buf[2] = b'0' + m as u8;
    buf[3] = b':';
    buf[4] = b'0' + (s / 10) as u8;
    buf[5] = b'0' + (s % 10) as u8;
    core::str::from_utf8(&buf[..6]).unwrap_or("")
}

fn format_town<'a>(buf: &'a mut [u8], alive: u32) -> &'a str {
    buf[..2].copy_from_slice(b"C ");
    buf[2] = b'0' + (alive % 10) as u8;
    buf[3] = b'/';
    buf[4] = b'6';
    core::str::from_utf8(&buf[..5]).unwrap_or("")
}

fn format_fire<'a>(buf: &'a mut [u8], sites: u32) -> &'a str {
    buf[..2].copy_from_slice(b"F ");
    let n = sites.min(999);
    let h = (n / 100) % 10;
    let t = (n / 10) % 10;
    let o = n % 10;
    let mut len = 2;
    if h > 0 { buf[len] = b'0' + h as u8; len += 1; }
    if h > 0 || t > 0 { buf[len] = b'0' + t as u8; len += 1; }
    buf[len] = b'0' + o as u8;
    len += 1;
    core::str::from_utf8(&buf[..len]).unwrap_or("")
}

fn format_unit<'a>(buf: &'a mut [u8], label: &str) -> &'a str {
    buf[..2].copy_from_slice(b"U ");
    let bytes = label.as_bytes();
    let n = bytes.len().min(buf.len() - 2);
    buf[2..2 + n].copy_from_slice(&bytes[..n]);
    core::str::from_utf8(&buf[..2 + n]).unwrap_or("")
}
