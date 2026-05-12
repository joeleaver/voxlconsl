//! Left-side sidebar built from four stacked `Screen`-mode actors,
//! each 32×32 (the SVO prefab cap). Sections from top to bottom:
//!
//! - **STATUS** — mission timer, town survival, fire site count.
//! - **UNIT** — selected unit name, state, heli bucket level.
//! - **ORDERS** — current orders the heli + crew are working on.
//! - **HELP** — static reminder of WSAD / wheel / J / K.
//!
//! Each section is its own actor with its own cache key, so the
//! STATUS section repainting once per displayed second doesn't
//! trigger a repaint of the static HELP block.
//!
//! All four actors share one 1024-byte dense prefab buffer: each
//! actor is born from the same all-air prefab, then we paint into
//! its volume with `actor_set_voxel`. Voxel coords inside the actor
//! follow the engine convention for Screen mode: local `+X` →
//! screen-right, local `+Y` → screen-up. Glyph row 0 (top of the
//! letter) maps to the largest local Y for its line.

use voxlconsl_sdk::*;
use voxlconsl_sdk::text::{Font, FONT_TINY};

use crate::units::UnitId;
use crate::M_HUD_TEXT;

// ── Panel geometry ────────────────────────────────────────────────

const PANEL_W: u32 = 32;
const PANEL_H: u32 = 32;
const PANEL_VOL_BYTES: usize = (PANEL_W * PANEL_H) as usize;

const PANEL_PREFAB: PrefabId = PrefabId(64);

const SIDEBAR_X: f32 = 3.0;
const SIDEBAR_GAP: f32 = 4.0;
const PANEL_LAYER:  f32 = 100.0;
const LINE_SPACING: u32 = 7;

const SIDEBAR_LINE_MAX: usize = 8;     // 8 chars × 4 px = 32 px ≤ panel width

static mut PANEL_DENSE: [u8; PANEL_VOL_BYTES] = [0; PANEL_VOL_BYTES];

// ── Section layout ────────────────────────────────────────────────

#[derive(Copy, Clone)]
enum Section {
    Status = 0,
    Unit   = 1,
    Orders = 2,
    Help   = 3,
}
const SECTION_COUNT: usize = 4;

/// Y position of each section, top to bottom.
fn section_y(s: Section) -> f32 {
    let s = s as u32;
    3.0 + (s as f32) * (PANEL_H as f32 + SIDEBAR_GAP)
}

// ── Cache keys (one per section) ──────────────────────────────────
//
// Tracking each section's input as a fixed-size tuple of bytes lets
// us `Option<Key> == Option<Key>` cheaply each frame and skip
// re-rasterising when nothing changed.

#[derive(Copy, Clone, PartialEq, Eq)]
struct StatusKey {
    t_sec:  u32,
    alive:  u32,
    fire:   u32,
}

#[derive(Copy, Clone, PartialEq, Eq)]
struct UnitKey {
    name:   [u8; 4],
    state:  [u8; 4],
    bucket: [u8; 4],
}

#[derive(Copy, Clone, PartialEq, Eq)]
struct OrdersKey {
    heli_active: bool,
    heli_x:      u16,
    heli_z:      u16,
    crew_active: bool,
    crew_x:      u16,
    crew_z:      u16,
}

// ── Hud state ─────────────────────────────────────────────────────

pub(crate) struct Hud {
    actors:       [Option<ActorId>; SECTION_COUNT],
    status_cache: Option<StatusKey>,
    unit_cache:   Option<UnitKey>,
    orders_cache: Option<OrdersKey>,
    help_painted: bool,
}

impl Hud {
    pub(crate) const fn new() -> Self {
        Self {
            actors: [None; SECTION_COUNT],
            status_cache: None,
            unit_cache:   None,
            orders_cache: None,
            help_painted: false,
        }
    }

    pub(crate) fn init(&mut self) {
        // Define the shared all-air panel prefab. Every section's
        // actor spawns from this and forks on first set_voxel.
        unsafe {
            prefab_define(
                PANEL_PREFAB,
                &*(&raw const PANEL_DENSE),
                U8Vec3::new(PANEL_W as u8, PANEL_H as u8, 1),
            );
        }
        let sections = [Section::Status, Section::Unit, Section::Orders, Section::Help];
        for s in sections {
            let id = actor_spawn_from(PANEL_PREFAB, Orientation::Up)
                .expect("ic HUD actor spawn");
            actor_set_render_mode(id, ActorRenderMode::Screen);
            actor_set_position(id, Vec3::new(SIDEBAR_X, section_y(s), PANEL_LAYER));
            self.actors[s as usize] = Some(id);
        }
    }

    /// Render every section. Cheap per-frame; each painter early-
    /// returns when its inputs haven't changed.
    pub(crate) fn paint(&mut self, ctx: HudCtx<'_>) {
        self.paint_status(&ctx);
        self.paint_unit(&ctx);
        self.paint_orders(&ctx);
        if !self.help_painted {
            self.paint_help();
            self.help_painted = true;
        }
    }

    fn paint_status(&mut self, ctx: &HudCtx<'_>) {
        let key = StatusKey {
            t_sec: ctx.time_left_ms / 1000,
            alive: ctx.alive_mask.count_ones(),
            fire:  ctx.fire_sites,
        };
        if self.status_cache == Some(key) { return; }
        self.status_cache = Some(key);
        let actor = match self.actors[Section::Status as usize] { Some(a) => a, None => return };
        actor_clear(actor);

        let mut buf = [b' '; SIDEBAR_LINE_MAX];
        let s = format_time(&mut buf, ctx.time_left_ms);
        paint_line(actor, &FONT_TINY, 0, M_HUD_TEXT, s);
        let s = format_town(&mut buf, key.alive);
        paint_line(actor, &FONT_TINY, 1, M_HUD_TEXT, s);
        let s = format_fire(&mut buf, key.fire);
        paint_line(actor, &FONT_TINY, 2, M_HUD_TEXT, s);
    }

    fn paint_unit(&mut self, ctx: &HudCtx<'_>) {
        let key = unit_key_from_ctx(ctx);
        if self.unit_cache == Some(key) { return; }
        self.unit_cache = Some(key);
        let actor = match self.actors[Section::Unit as usize] { Some(a) => a, None => return };
        actor_clear(actor);

        let mut buf = [b' '; SIDEBAR_LINE_MAX];
        paint_line(actor, &FONT_TINY, 0, M_HUD_TEXT, "UNIT");
        let s = label_value(&mut buf, "  ", &key.name);
        paint_line(actor, &FONT_TINY, 1, M_HUD_TEXT, s);
        let s = label_value(&mut buf, "ST", &key.state);
        paint_line(actor, &FONT_TINY, 2, M_HUD_TEXT, s);
        if ctx.selected == Some(UnitId::Heli) {
            let s = label_value(&mut buf, "BK", &key.bucket);
            paint_line(actor, &FONT_TINY, 3, M_HUD_TEXT, s);
        }
    }

    fn paint_orders(&mut self, ctx: &HudCtx<'_>) {
        let key = OrdersKey {
            heli_active: ctx.heli_target.is_some(),
            heli_x:      ctx.heli_target.map(|(x, _)| x as u16).unwrap_or(0),
            heli_z:      ctx.heli_target.map(|(_, z)| z as u16).unwrap_or(0),
            crew_active: ctx.crew_target.is_some(),
            crew_x:      ctx.crew_target.map(|(x, _)| x as u16).unwrap_or(0),
            crew_z:      ctx.crew_target.map(|(_, z)| z as u16).unwrap_or(0),
        };
        if self.orders_cache == Some(key) { return; }
        self.orders_cache = Some(key);
        let actor = match self.actors[Section::Orders as usize] { Some(a) => a, None => return };
        actor_clear(actor);

        let mut buf = [b' '; SIDEBAR_LINE_MAX];
        paint_line(actor, &FONT_TINY, 0, M_HUD_TEXT, "ORDERS");
        let line = format_order(&mut buf, b'H', ctx.heli_target);
        paint_line(actor, &FONT_TINY, 1, M_HUD_TEXT, line);
        let line = format_order(&mut buf, b'C', ctx.crew_target);
        paint_line(actor, &FONT_TINY, 2, M_HUD_TEXT, line);
    }

    fn paint_help(&mut self) {
        let actor = match self.actors[Section::Help as usize] { Some(a) => a, None => return };
        actor_clear(actor);
        paint_line(actor, &FONT_TINY, 0, M_HUD_TEXT, "WSAD PAN");
        paint_line(actor, &FONT_TINY, 1, M_HUD_TEXT, "WHL ZOOM");
        paint_line(actor, &FONT_TINY, 2, M_HUD_TEXT, "J ORDER");
        paint_line(actor, &FONT_TINY, 3, M_HUD_TEXT, "K SEL");
    }
}

/// Per-frame inputs the cart hands to `Hud::paint`. Bundled into a
/// struct so the call site doesn't drift if we add more fields.
pub(crate) struct HudCtx<'a> {
    pub time_left_ms: u32,
    pub alive_mask:   u32,
    pub fire_sites:   u32,
    pub selected:     Option<UnitId>,
    pub unit_label:   &'a str,
    pub unit_state:   &'a str,
    pub heli_bucket:  &'a str,
    pub heli_target:  Option<(u32, u32)>,
    pub crew_target:  Option<(u32, u32)>,
}

fn unit_key_from_ctx(ctx: &HudCtx<'_>) -> UnitKey {
    let mut name = [b' '; 4];
    let mut state = [b' '; 4];
    let mut bucket = [b' '; 4];
    copy_label(&mut name, ctx.unit_label);
    copy_label(&mut state, ctx.unit_state);
    copy_label(&mut bucket, ctx.heli_bucket);
    UnitKey { name, state, bucket }
}

fn copy_label(dst: &mut [u8; 4], src: &str) {
    let bytes = src.as_bytes();
    let n = bytes.len().min(4);
    dst[..n].copy_from_slice(&bytes[..n]);
    for b in &mut dst[n..] { *b = b' '; }
}

// ── Glyph painter ─────────────────────────────────────────────────

/// Paint `text` on a horizontal line of a 32×32 panel actor. Line 0
/// is the top row of the panel.
fn paint_line(
    actor: ActorId,
    font: &Font<'_>,
    line_idx: u32,
    color: u8,
    text: &str,
) {
    let cell_w = font.cell_width() as u32;
    let cell_h = font.cell_height() as u32;
    // Top-of-line in actor-local Y, accounting for the 1-px top margin.
    let top_y = (PANEL_H - 1).saturating_sub(1 + line_idx * LINE_SPACING);
    if top_y < cell_h - 1 { return; }

    let mut col_offset = 0u32;
    for ch in text.chars() {
        let cp = ch as u32;
        for row in 0..cell_h {
            for col in 0..cell_w {
                if !font.glyph_bit(cp, col as u8, row as u8) { continue; }
                let x = col_offset + col;
                let y = top_y - row;
                if x >= PANEL_W { continue; }
                actor_set_voxel(actor, U8Vec3::new(x as u8, y as u8, 0), color);
            }
        }
        col_offset += cell_w;
    }
}

// ── Formatters (no_std + no_alloc) ────────────────────────────────

fn format_time<'a>(buf: &'a mut [u8], t_ms: u32) -> &'a str {
    let sec = t_ms / 1000;
    let m = (sec / 60) % 10;
    let s = sec % 60;
    buf[..3].copy_from_slice(b"TM ");
    buf[3] = b'0' + m as u8;
    buf[4] = b':';
    buf[5] = b'0' + (s / 10) as u8;
    buf[6] = b'0' + (s % 10) as u8;
    core::str::from_utf8(&buf[..7]).unwrap_or("")
}

fn format_town<'a>(buf: &'a mut [u8], alive: u32) -> &'a str {
    buf[..3].copy_from_slice(b"TN ");
    buf[3] = b'0' + (alive % 10) as u8;
    buf[4] = b'/';
    buf[5] = b'6';
    core::str::from_utf8(&buf[..6]).unwrap_or("")
}

fn format_fire<'a>(buf: &'a mut [u8], sites: u32) -> &'a str {
    buf[..3].copy_from_slice(b"FR ");
    let n = sites.min(999);
    let h = (n / 100) % 10;
    let t = (n / 10) % 10;
    let o = n % 10;
    let mut len = 3;
    if h > 0 { buf[len] = b'0' + h as u8; len += 1; }
    if h > 0 || t > 0 { buf[len] = b'0' + t as u8; len += 1; }
    buf[len] = b'0' + o as u8;
    len += 1;
    core::str::from_utf8(&buf[..len]).unwrap_or("")
}

fn label_value<'a>(buf: &'a mut [u8], label: &str, value: &[u8; 4]) -> &'a str {
    let lb = label.as_bytes();
    let lb_n = lb.len().min(3);
    buf[..lb_n].copy_from_slice(&lb[..lb_n]);
    buf[lb_n] = b' ';
    let mut len = lb_n + 1;
    for &b in value.iter() {
        if b == b' ' && len == lb_n + 1 { continue; }   // trim leading spaces
        if len >= buf.len() { break; }
        buf[len] = b;
        len += 1;
    }
    // trim trailing spaces
    while len > 0 && buf[len - 1] == b' ' { len -= 1; }
    core::str::from_utf8(&buf[..len]).unwrap_or("")
}

/// "H>123,4" formatted into the 8-char line budget. `kind` is the
/// unit's single-letter prefix; `target` is the order's destination
/// cell. Idle units render as just "H -" / "C -".
fn format_order<'a>(buf: &'a mut [u8], kind: u8, target: Option<(u32, u32)>) -> &'a str {
    buf[0] = kind;
    buf[1] = b' ';
    match target {
        None => {
            buf[2] = b'-';
            core::str::from_utf8(&buf[..3]).unwrap_or("")
        }
        Some((x, z)) => {
            let mut len = 2;
            len += write_u16_compact(&mut buf[len..], x as u16);
            if len >= buf.len() { return core::str::from_utf8(&buf[..len]).unwrap_or(""); }
            buf[len] = b',';
            len += 1;
            len += write_u16_compact(&mut buf[len..], z as u16);
            core::str::from_utf8(&buf[..len]).unwrap_or("")
        }
    }
}

/// Write a u16 without leading zeros into `buf`. Returns bytes written.
fn write_u16_compact(buf: &mut [u8], mut n: u16) -> usize {
    if n == 0 {
        if !buf.is_empty() { buf[0] = b'0'; return 1; } else { return 0; }
    }
    let mut tmp = [0u8; 5];
    let mut t = 0;
    while n > 0 && t < tmp.len() {
        tmp[t] = b'0' + (n % 10) as u8;
        n /= 10;
        t += 1;
    }
    let written = t.min(buf.len());
    for i in 0..written {
        buf[i] = tmp[t - 1 - i];
    }
    written
}
