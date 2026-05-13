//! Left-side sidebar built from four stacked `Screen`-mode actors,
//! each 32×32 (the SVO prefab cap). Sections from top to bottom:
//!
//! - **STATUS** — mission timer, town survival, fire site count.
//! - **UNIT** — selected unit name, state, heli bucket level.
//! - **ORDERS** — current orders the heli + crew are working on.
//! - **HELP** — controls cheat-sheet built from the host-provided
//!   binding labels (§6.5), so the displayed keys follow the active
//!   binding instead of being hard-coded.
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
    t_sec:    u32,
    alive:    u32,
    fire:     u32,
    wind_dir: [u8; 2],   // padded with space when 1-letter
    wind_str: u32,
}

#[derive(Copy, Clone, PartialEq, Eq)]
struct UnitKey {
    heli_busy:    u32,
    heli_total:   u32,
    crew_busy:    u32,
    crew_total:   u32,
    hotshot_busy: u32,
    hotshot_total: u32,
    line_active:  bool,
    line_count:   u32,
    queue_total:  u32,
}

#[derive(Copy, Clone, PartialEq, Eq)]
struct OrdersKey {
    tier:       u32,
    heli_total: u32,
    crew_total: u32,
}

/// Snapshot of the four labels the HELP section paints. Repaint only
/// fires when one of these changes (device switch / rebind).
const LABEL_CAP: usize = 16;

#[derive(Copy, Clone, PartialEq, Eq)]
struct HelpKey {
    pan:     [u8; LABEL_CAP],
    primary: [u8; LABEL_CAP],
    confirm: [u8; LABEL_CAP],
    cancel:  [u8; LABEL_CAP],
}

// ── Hud state ─────────────────────────────────────────────────────

pub(crate) struct Hud {
    actors:       [Option<ActorId>; SECTION_COUNT],
    status_cache: Option<StatusKey>,
    unit_cache:   Option<UnitKey>,
    orders_cache: Option<OrdersKey>,
    help_cache:   Option<HelpKey>,
}

impl Hud {
    pub(crate) const fn new() -> Self {
        Self {
            actors: [None; SECTION_COUNT],
            status_cache: None,
            unit_cache:   None,
            orders_cache: None,
            help_cache:   None,
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
        self.paint_help(&ctx);
    }

    fn paint_status(&mut self, ctx: &HudCtx<'_>) {
        let wind_bytes = ctx.wind_dir.as_bytes();
        let mut wind_dir = [b' '; 2];
        for i in 0..2.min(wind_bytes.len()) { wind_dir[i] = wind_bytes[i]; }
        let key = StatusKey {
            t_sec:    ctx.time_left_ms / 1000,
            alive:    ctx.alive_mask.count_ones(),
            fire:     ctx.fire_sites,
            wind_dir,
            wind_str: ctx.wind_strength,
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
        let s = format_wind(&mut buf, ctx.wind_dir, ctx.wind_strength);
        paint_line(actor, &FONT_TINY, 3, M_HUD_TEXT, s);
    }

    fn paint_unit(&mut self, ctx: &HudCtx<'_>) {
        let key = UnitKey {
            heli_busy:    ctx.heli_busy,
            heli_total:   ctx.heli_total,
            crew_busy:    ctx.crew_busy,
            crew_total:   ctx.crew_total,
            hotshot_busy: ctx.hotshot_busy,
            hotshot_total: ctx.hotshot_total,
            line_active:  ctx.line_mode_active,
            line_count:   ctx.line_mode_count,
            queue_total:  ctx.queue_total,
        };
        if self.unit_cache == Some(key) { return; }
        self.unit_cache = Some(key);
        let actor = match self.actors[Section::Unit as usize] { Some(a) => a, None => return };
        actor_clear(actor);

        let mut buf = [b' '; SIDEBAR_LINE_MAX];
        // Line 0: heli pool busy/total — "H 1/3".
        let s = format_pool(&mut buf, b'H', ctx.heli_busy, ctx.heli_total);
        paint_line(actor, &FONT_TINY, 0, M_HUD_TEXT, s);
        // Line 1: crew pool busy/total — "C 2/6".
        let s = format_pool(&mut buf, b'C', ctx.crew_busy, ctx.crew_total);
        paint_line(actor, &FONT_TINY, 1, M_HUD_TEXT, s);
        // Line 2: hot-shot crew pool busy/total — "S 4/8".
        let s = format_pool(&mut buf, b'S', ctx.hotshot_busy, ctx.hotshot_total);
        paint_line(actor, &FONT_TINY, 2, M_HUD_TEXT, s);
        // Line 3: line draft size when drafting; total queue size
        // otherwise. (Per-order map badges show the queue itself.)
        let s = if ctx.line_mode_active {
            format_line_count(&mut buf, ctx.line_mode_count)
        } else {
            format_queue_total(&mut buf, ctx.queue_total)
        };
        paint_line(actor, &FONT_TINY, 3, M_HUD_TEXT, s);
    }

    fn paint_orders(&mut self, ctx: &HudCtx<'_>) {
        let key = OrdersKey {
            tier:       ctx.tier,
            heli_total: ctx.heli_total,
            crew_total: ctx.crew_total,
        };
        if self.orders_cache == Some(key) { return; }
        self.orders_cache = Some(key);
        let actor = match self.actors[Section::Orders as usize] { Some(a) => a, None => return };
        actor_clear(actor);

        let mut buf = [b' '; SIDEBAR_LINE_MAX];
        let s = format_tier(&mut buf, ctx.tier);
        paint_line(actor, &FONT_TINY, 0, M_HUD_TEXT, s);
        let s = format_budget(&mut buf, b'H', ctx.heli_total);
        paint_line(actor, &FONT_TINY, 1, M_HUD_TEXT, s);
        let s = format_budget(&mut buf, b'C', ctx.crew_total);
        paint_line(actor, &FONT_TINY, 2, M_HUD_TEXT, s);
    }

    fn paint_help(&mut self, ctx: &HudCtx<'_>) {
        let key = HelpKey {
            pan:     label_to_key(ctx.pan_label),
            primary: label_to_key(ctx.primary_label),
            confirm: label_to_key(ctx.confirm_label),
            cancel:  label_to_key(ctx.cancel_label),
        };
        if self.help_cache == Some(key) { return; }
        self.help_cache = Some(key);
        let actor = match self.actors[Section::Help as usize] { Some(a) => a, None => return };
        actor_clear(actor);

        let mut buf = [b' '; SIDEBAR_LINE_MAX];
        let s = format_help_line(&mut buf, ctx.pan_label,     "MOV");
        paint_line(actor, &FONT_TINY, 0, M_HUD_TEXT, s);
        let s = format_help_line(&mut buf, ctx.primary_label, "ACT");
        paint_line(actor, &FONT_TINY, 1, M_HUD_TEXT, s);
        let s = format_help_line(&mut buf, ctx.confirm_label, "OK");
        paint_line(actor, &FONT_TINY, 2, M_HUD_TEXT, s);
        let s = format_help_line(&mut buf, ctx.cancel_label,  "X");
        paint_line(actor, &FONT_TINY, 3, M_HUD_TEXT, s);
    }
}

/// Right-pad-truncate a label into a fixed-size key for the HELP
/// cache. Trailing zeroes act as terminators so a "J" key never
/// compares equal to a "JK" one.
fn label_to_key(label: &str) -> [u8; LABEL_CAP] {
    let mut out = [0u8; LABEL_CAP];
    let bytes = label.as_bytes();
    let n = bytes.len().min(LABEL_CAP);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// Format `"<label> <verb>"` into `buf`, truncating at the panel
/// width so a long label doesn't bleed past the sidebar.
fn format_help_line<'a>(buf: &'a mut [u8; SIDEBAR_LINE_MAX], label: &str, verb: &str) -> &'a str {
    let mut i = 0;
    for &b in label.as_bytes() {
        if i >= buf.len() { break; }
        buf[i] = b; i += 1;
    }
    if i < buf.len() && !verb.is_empty() {
        buf[i] = b' '; i += 1;
    }
    for &b in verb.as_bytes() {
        if i >= buf.len() { break; }
        buf[i] = b; i += 1;
    }
    core::str::from_utf8(&buf[..i]).unwrap_or("")
}

/// Per-frame inputs the cart hands to `Hud::paint`. Bundled into a
/// struct so the call site doesn't drift if we add more fields.
pub(crate) struct HudCtx<'a> {
    pub time_left_ms:  u32,
    pub alive_mask:    u32,
    pub fire_sites:    u32,
    pub wind_dir:      &'a str,
    pub wind_strength: u32,
    pub heli_busy:     u32,
    pub heli_total:    u32,
    pub crew_busy:     u32,
    pub crew_total:    u32,
    pub hotshot_busy:  u32,
    pub hotshot_total: u32,
    pub tier:          u32,
    pub line_mode_active: bool,
    pub line_mode_count:  u32,
    pub queue_total:   u32,
    pub pan_label:     &'a str,
    pub primary_label: &'a str,
    pub confirm_label: &'a str,
    pub cancel_label:  &'a str,
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

/// `WD SE 3` — direction (1-2 chars) followed by single-digit
/// strength. Eight chars max so it fits a 32-wide panel at 4 px /
/// glyph.
fn format_wind<'a>(buf: &'a mut [u8], dir: &str, strength: u32) -> &'a str {
    buf[..3].copy_from_slice(b"WD ");
    let dir_bytes = dir.as_bytes();
    let n = dir_bytes.len().min(2);
    buf[3..3 + n].copy_from_slice(&dir_bytes[..n]);
    let mut len = 3 + n;
    if len < buf.len() { buf[len] = b' '; len += 1; }
    if len < buf.len() {
        buf[len] = b'0' + (strength.min(9)) as u8;
        len += 1;
    }
    core::str::from_utf8(&buf[..len]).unwrap_or("")
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

/// "H 1/3" — pool busy/total for a unit type.
fn format_pool<'a>(buf: &'a mut [u8], kind: u8, busy: u32, total: u32) -> &'a str {
    buf[0] = kind;
    buf[1] = b' ';
    buf[2] = b'0' + (busy.min(9)) as u8;
    buf[3] = b'/';
    buf[4] = b'0' + (total.min(9)) as u8;
    core::str::from_utf8(&buf[..5]).unwrap_or("")
}

/// "TIER N" — single line with the mission's tier number.
fn format_tier<'a>(buf: &'a mut [u8], tier: u32) -> &'a str {
    buf[..5].copy_from_slice(b"TIER ");
    buf[5] = b'0' + (tier.min(9)) as u8;
    core::str::from_utf8(&buf[..6]).unwrap_or("")
}

/// "H BUD 3" — unit-type budget for the mission.
fn format_budget<'a>(buf: &'a mut [u8], kind: u8, total: u32) -> &'a str {
    buf[0] = kind;
    buf[..1].copy_from_slice(&[kind]);
    buf[1..6].copy_from_slice(b" BUD ");
    buf[6] = b'0' + (total.min(9)) as u8;
    core::str::from_utf8(&buf[..7]).unwrap_or("")
}

/// "LN N/8" — current draft size, max LINE_CAP=8.
fn format_line_count<'a>(buf: &'a mut [u8], n: u32) -> &'a str {
    buf[..3].copy_from_slice(b"LN ");
    buf[3] = b'0' + (n.min(9)) as u8;
    buf[4] = b'/';
    buf[5] = b'8';
    core::str::from_utf8(&buf[..6]).unwrap_or("")
}

/// "Q NN" — total pending orders across both unit types.
fn format_queue_total<'a>(buf: &'a mut [u8], n: u32) -> &'a str {
    buf[..2].copy_from_slice(b"Q ");
    let nn = n.min(99);
    let tens = nn / 10;
    let ones = nn % 10;
    let mut len = 2;
    if tens > 0 { buf[len] = b'0' + tens as u8; len += 1; }
    buf[len] = b'0' + ones as u8;
    len += 1;
    core::str::from_utf8(&buf[..len]).unwrap_or("")
}

