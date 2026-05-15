//! Title screen — "INCIDENT COMMANDER" rendered as worldspace voxel
//! letters. The "C" in INCIDENT (the top line) catches fire; a
//! helicopter flies in from the east high above the title, parks
//! above the C, and dumps a tall column of water to put it out.
//! Once the fire is gone, the menu becomes interactive: W/D toggles
//! between STORY MODE and ENDLESS MODE, J confirms.
//!
//! The title runs in its own scene (`SCENE_TITLE`) so it doesn't share
//! voxels with the gameplay world. Gameplay lives in `SCENE_GAME` and
//! is only constructed once the player picks a mode.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics::material_at;
use voxlconsl_sdk::text::{measure, paint_world as paint_text, Axis, FONT_DCP1, FONT_TINY};

use crate::mathlib::sine;
use crate::rng::Rng;
use crate::{
    M_EMBER, M_HELICOPTER_BODY, M_HELICOPTER_ROTOR, M_HUD_TEXT, M_SCORCH,
    M_STONE, M_TITLE_FLAME, M_TITLE_LIGHTNING, M_TITLE_MENU, M_TITLE_MENU_BG,
    M_WATER,
};

// ── World layout (title scene) ────────────────────────────────────
//
// Letters sit high in the otherwise-empty title scene so the camera
// can frame them against the sky.

const TITLE_SCALE: u8  = 2;
const TITLE_DEPTH: u32 = 8;

/// Vertical position of the title's center (between the two lines).
const TITLE_MID_Y: u32 = 220;
const TITLE_GAP:   u32 = 8;

/// Common Z position for all title voxels — single 3D slab.
const TITLE_Z: u32 = 256;

const LINE_INCIDENT:  &str = "INCIDENT";
const LINE_COMMANDER: &str = "COMMANDER";

// ── Helicopter ────────────────────────────────────────────────────

const HELI_SX: u8 = 5;
const HELI_SY: u8 = 4;
const HELI_SZ: u8 = 5;

/// Constant altitude for the title heli — well above the letter tops
/// so the water drop has room to fall and reads as a real column.
const HELI_FLY_Y: f32 = (TITLE_MID_Y + 75) as f32;
/// X the heli enters from (east, +X) and where it exits to (west, -X).
const HELI_START_X: f32 = 420.0;
const HELI_END_X:   f32 = 92.0;
/// Fallback X for the heli's drop position if the runtime calc fails
/// (which it shouldn't). Real value is recomputed at `init` via
/// proportional layout so the heli always lands on the "C" of
/// INCIDENT regardless of font kerning changes.
const HELI_DROP_X_FALLBACK: f32 = 208.0;
/// Voxels per second the heli moves horizontally.
const HELI_SPEED:   f32 = 90.0;

// ── Sim tuning ────────────────────────────────────────────────────

/// Max fire seed positions stored at boot. Each position paints a
/// 3×4×3 cluster (36 voxels) of M_TITLE_FLAME — taller than wide so
/// it reads as a flame rather than a blob, and big enough to occupy
/// several screen pixels at the title's render distance. Cleared by
/// the heli's drop cell by cell. Generous count to cover the C's
/// full width with varied flame heights.
const SEED_FIRE_COUNT: usize = 60;
/// Cluster shape painted per seed cell (X-width, Y-height, Z-depth).
/// Anchor is the bottom-front-left corner of the cluster.
const FIRE_CLUSTER_W: u32 = 3;
const FIRE_CLUSTER_H: u32 = 4;
const FIRE_CLUSTER_D: u32 = 3;
/// Ms the heli spends parked over the drop X painting water before
/// resuming westward.
const DROP_DURATION_MS: u32 = 1_500;
/// Ms after the heli has departed before the menu becomes interactive.
/// Gives the smoke / aftermath a beat to settle.
const SETTLE_MS: u32 = 600;

// ── Intro animation timing ────────────────────────────────────────
//
// Order of events: splash dismiss → bolt strikes the C → bolt
// fades → fire spreads across the C cell-by-cell → heli enters.

/// Ms the lightning bolt voxels are painted before being cleared.
/// Short — just a flash, then the strike afterglow is the lit C.
const LIGHTNING_FLASH_MS: u32 = 180;
/// Beat between the bolt clearing and the first flame appearing — a
/// brief breath that reads as "moment of impact".
const LIGHTNING_HOLD_MS:  u32 = 140;
/// Total time the fire spends spreading from 0 → all seed cells lit.
const FIRE_SPREAD_MS:     u32 = 2_000;
/// Extra raging-fire time after the full burn before the heli arrives.
const BURNING_HOLD_MS:    u32 = 1_200;
/// Cap on the number of voxels painted for the bolt geometry. At the
/// title's render distance ~1.25 voxels fit in one screen pixel, so
/// each bolt segment paints a 3×3×3 cluster (27 voxels) to read as
/// ~2 px in screen space. ~16 segments × 27 ≈ 432.
const LIGHTNING_BOLT_CAP: usize = 512;

// ── Ember + scorch ────────────────────────────────────────────────
//
// While the C is burning, embers spawn from its crown and drift up,
// and random body voxels get charred to M_SCORCH so the letter looks
// progressively eaten by the fire. The heli's water column stops new
// spawns/scorching at the start of Dropping; existing embers age out
// during the drop, and scorch marks stay (visible damage).

const EMBER_POOL:        usize = 10;
const EMBER_LIFE_TICKS:  u8    = 36;     // ~0.6 s at 60 fps
const EMBER_SPAWN_MS:    u32   = 70;
const SCORCH_INTERVAL_MS: u32  = 140;
const MAX_SCORCH:        usize = 14;     // leaves the C still legible

// ── Menu actors ───────────────────────────────────────────────────
//
// One Screen-mode actor per option. Prefab dimensions are capped at
// the SVO build CHUNK_SIZE (=32) per axis, so panels are 32×16. Labels
// use FONT_TINY (4×6 cell) — "STORY" = 20 px, "ENDLESS" = 28 px, both
// fit a 32 px panel.

const MENU_W: u32 = 32;
const MENU_H: u32 = 16;
const MENU_PREFAB: PrefabId = PrefabId(80);
const MENU_PREFAB_VOL: usize = (MENU_W * MENU_H) as usize;
static mut MENU_DENSE: [u8; MENU_PREFAB_VOL] = [0; MENU_PREFAB_VOL];

/// Screen positions. Centered horizontally on the 256-wide framebuffer.
const MENU_X:         f32 = 112.0;          // (256 - 32) / 2
const MENU_STORY_Y:   f32 = 92.0;
const MENU_ENDLESS_Y: f32 = 112.0;

// ── Splash actor ──────────────────────────────────────────────────
//
// Shown until the player presses anything. The cart can't `music_play`
// before a user gesture (browser autoplay policy gates the
// `AudioContext`), so we hold off the title animation + music until
// the splash dismisses. That way the heli flying in and the theme
// kicking in land together.

const SPLASH_PREFAB: PrefabId = PrefabId(81);
const SPLASH_PREFAB_VOL: usize = (MENU_W * MENU_H) as usize;
static mut SPLASH_DENSE: [u8; SPLASH_PREFAB_VOL] = [0; SPLASH_PREFAB_VOL];
const SPLASH_X: f32 = 112.0; // (256 - 32) / 2
const SPLASH_Y: f32 = 120.0; // bottom-center, below title

// ── State machine ─────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq)]
enum Phase {
    /// Waiting for the first user input. Browser autoplay policy
    /// blocks audio until a gesture, so we also hold the heli + ember
    /// + scorch ticking off until the player engages — that way the
    /// theme music and the title animation land together.
    Splash,
    /// Lightning bolt is painted striking the C of INCIDENT. No
    /// flames yet — the strike precedes the fire.
    Lightning,
    /// Title is on fire, heli has not yet appeared. Fire spreads
    /// outward across the C over `FIRE_SPREAD_MS` before the heli
    /// arrives — flames don't all light at once.
    Burning,
    /// Heli is flying in from the east.
    Approaching,
    /// Heli parked above the title, painting water + clearing fire.
    Dropping,
    /// Heli flying westward off-screen. Fire is out.
    Departing,
    /// Menu is interactive; we're waiting on a selection.
    MenuReady,
}

/// User picks 0 = Story, 1 = Endless.
pub(crate) const PICK_STORY:   u8 = 0;
pub(crate) const PICK_ENDLESS: u8 = 1;

#[derive(Copy, Clone)]
struct TitleEmber {
    alive:    bool,
    age:      u8,
    /// Last cell we painted M_EMBER into — cleared at the next step
    /// (or when the ember dies).
    last:     UVec3,
    pos:      Vec3,
    vel:      Vec3,
}

impl TitleEmber {
    const DEAD: Self = Self {
        alive: false,
        age:   0,
        last:  UVec3::ZERO,
        pos:   Vec3::ZERO,
        vel:   Vec3::ZERO,
    };
}

pub(crate) struct Title {
    heli_actor:      Option<ActorId>,
    menu_story:      Option<ActorId>,
    menu_endless:    Option<ActorId>,
    splash_actor:    Option<ActorId>,
    phase:           Phase,
    phase_ms:        u32,
    heli_x:          f32,
    rotor_phase:     u8,
    /// Menu selection: 0 or 1.
    selected:        u8,
    /// Latched once the user confirms; lib.rs reads this and starts gameplay.
    confirmed:       Option<u8>,
    /// Cache key for menu repaint = (selected, ready).
    menu_cache_key:  u16,
    /// Persistent fire-cell positions, painted once at boot with
    /// M_TITLE_FLAME (no CA decay). The Dropping phase clears them
    /// progressively as the heli drops water.
    fire_cells:      [UVec3; SEED_FIRE_COUNT],
    /// Cells still lit (decrements during Dropping).
    fire_count:      u8,
    /// Original count captured at seed time, used as the denominator
    /// for the linear extinguish schedule.
    fire_total:      u8,
    /// Ember pool — drifts up from the C while it burns.
    embers:          [TitleEmber; EMBER_POOL],
    /// Cumulative ms since `init` — drives ember/scorch cadence
    /// independently of per-phase `phase_ms`.
    total_ms:        u32,
    /// `total_ms` of the most recent ember spawn.
    last_ember_ms:   u32,
    /// `total_ms` of the most recent scorch mark.
    last_scorch_ms:  u32,
    /// Number of scorch marks placed so far.
    scorch_count:    u8,
    /// World X where the heli parks to drop water — center of the C
    /// of INCIDENT in proportional layout (computed at `init`).
    heli_drop_x:     f32,
    /// World X of the C's leftmost ink column (lower bound for fire /
    /// scorch / ember sampling) and its ink width.
    c_world_x:       u32,
    c_world_w:       u32,
    /// Cells painted with M_TITLE_LIGHTNING during `Phase::Lightning`,
    /// cleared at the end of the flash.
    bolt_cells:      [UVec3; LIGHTNING_BOLT_CAP],
    bolt_count:      u16,
    rng:             Rng,
}

impl Title {
    pub(crate) const fn new() -> Self {
        Self {
            heli_actor:     None,
            menu_story:     None,
            menu_endless:   None,
            splash_actor:   None,
            phase:          Phase::Splash,
            phase_ms:       0,
            heli_x:         HELI_START_X,
            rotor_phase:    0,
            selected:       PICK_STORY,
            confirmed:      None,
            menu_cache_key: 0xFFFF,
            fire_cells:     [UVec3::ZERO; SEED_FIRE_COUNT],
            fire_count:     0,
            fire_total:     0,
            embers:         [TitleEmber::DEAD; EMBER_POOL],
            total_ms:       0,
            last_ember_ms:  0,
            last_scorch_ms: 0,
            scorch_count:   0,
            heli_drop_x:    HELI_DROP_X_FALLBACK,
            c_world_x:      0,
            c_world_w:      0,
            bolt_cells:     [UVec3::ZERO; LIGHTNING_BOLT_CAP],
            bolt_count:     0,
            rng:            Rng(0xC0FFEE_77),
        }
    }

    /// Paint the title geometry, spawn the heli + menu actors.
    /// Assumes the title scene is already active. Music + animation
    /// stay paused until the first user input — `tick` calls
    /// `audio::init_title_music` and transitions to `Burning` the
    /// frame the player engages.
    pub(crate) fn init(&mut self) {
        // Sunset gradient — magenta-pink overhead lerps down through
        // warm reds to a bright golden horizon. The renderer mixes
        // top + horizon in RGB space, so pairs whose mid-blend stays
        // warm (pink/magenta + orange = coral/peach) read more like
        // a classic sunset than dark-blue + orange (which mixes to
        // muddy brown).
        sky_set_gradient(
            Material::pack_color(13, 2),  // magenta:2 — dusky pink overhead
            Material::pack_color(11, 3),  // orange:3 — golden hour
        );
        clear_world();
        paint_title_text();
        // Locate the "C" of INCIDENT under whatever kerning/layout the
        // font currently produces.
        let inc_origin = line_origin(LINE_INCIDENT, true);
        self.c_world_x = glyph_world_x(LINE_INCIDENT, 2, inc_origin.x);
        self.c_world_w = glyph_world_width('C');
        self.heli_drop_x = (self.c_world_x + self.c_world_w / 2) as f32;
        self.seed_fire();
        self.spawn_heli();
        self.spawn_menu();
        self.spawn_splash();
    }

    fn spawn_splash(&mut self) {
        unsafe {
            prefab_define(
                SPLASH_PREFAB,
                &*(&raw const SPLASH_DENSE),
                U8Vec3::new(MENU_W as u8, MENU_H as u8, 1),
            );
        }
        let actor = actor_spawn_from(SPLASH_PREFAB, Orientation::Up).expect("splash actor");
        actor_set_render_mode(actor, ActorRenderMode::Screen);
        actor_set_position(actor, Vec3::new(SPLASH_X, SPLASH_Y, 100.0));
        // Paint the badge using the same dark-fill + cyan-border style
        // as the highlighted menu option for a consistent visual.
        paint_menu_panel(actor, "PRESS J", true);
        self.splash_actor = Some(actor);
    }

    fn seed_fire(&mut self) {
        // Precompute cluster positions across the upper two-thirds of
        // the C's body. Nothing is painted here; the `Burning` phase
        // progressively reveals these cells over `FIRE_SPREAD_MS` so
        // the fire visibly spreads after the bolt hits. Glyph coords
        // are scale=1 (raw bitmap); world coords use TITLE_SCALE.
        let inc_origin = line_origin(LINE_INCIDENT, true);
        let cell_h = FONT_DCP1.cell_height() as u32;
        let scale  = TITLE_SCALE as u32;
        let (c_left, _c_right) = FONT_DCP1
            .glyph_ink_bounds('C' as u32)
            .unwrap_or((0, FONT_DCP1.cell_width() - 1));
        let top_y = inc_origin.y + cell_h * scale;

        // Place flame clusters ON the C's ink, painted at the front of
        // the letter's z-slab so the camera (which mostly sees the C's
        // front face) sees fire consuming the letter, not just sitting
        // on top of it.
        //
        // Anchor of each cluster is the bottom-front-left corner; the
        // 3×4×3 cluster extends up and into the letter's z range.
        let cluster_z = inc_origin.z + TITLE_DEPTH.saturating_sub(FIRE_CLUSTER_D);

        let cap = self.fire_cells.len().min(SEED_FIRE_COUNT);
        let mut placed = 0;
        let mut attempts = 0;
        while placed < cap && attempts < cap * 16 {
            attempts += 1;
            let col_in_ink = self.rng.range(self.c_world_w / scale);
            let col_in_char = (col_in_ink + c_left as u32) as u8;
            // Sample any row of the glyph — flames spread across the
            // whole C ink, not just the top arc.
            let row_in_char = self.rng.range(cell_h) as u8;
            if !FONT_DCP1.glyph_bit('C' as u32, col_in_char, row_in_char) {
                continue;
            }
            let world_x = self.c_world_x + col_in_ink * scale + self.rng.range(scale);
            // Anchor at the glyph cell row's bottom-y in world coords.
            // The 4-tall cluster reaches up into the row above, so the
            // flame appears to lick upward off the letter's ink.
            let row_y = inc_origin.y + (cell_h - 1 - row_in_char as u32) * scale;
            let y_jit = self.rng.range(scale + 2);
            let world_y = row_y + y_jit;
            // Don't let flames extend above the top of the letter by
            // more than 6 voxels — keeps the flame anchored to the
            // letter rather than floating in the sky.
            let world_y = world_y.min(top_y + 4);
            self.fire_cells[placed] = UVec3::new(world_x, world_y, cluster_z);
            placed += 1;
        }
        // No cells lit yet — `Burning` reveals them over time.
        self.fire_count = 0;
        self.fire_total = placed as u8;
    }

    /// Paint or clear the flame cluster anchored at a seed-cell
    /// position. `mat = 0` extinguishes. Cluster extends upward (in
    /// +Y) from the anchor so flames read as columns rising off the
    /// letter.
    fn fire_cluster_set(anchor: UVec3, mat: u8) {
        for ox in 0..FIRE_CLUSTER_W {
            for oy in 0..FIRE_CLUSTER_H {
                for oz in 0..FIRE_CLUSTER_D {
                    set_voxel(
                        UVec3::new(anchor.x + ox, anchor.y + oy, anchor.z + oz),
                        mat,
                    );
                }
            }
        }
    }

    /// Clear every stored fire-cell to air. Called once the heli has
    /// finished its drop so the title settles into "flames out".
    fn extinguish_all_fire(&mut self) {
        for i in 0..self.fire_count as usize {
            Self::fire_cluster_set(self.fire_cells[i], 0);
        }
        self.fire_count = 0;
    }

    /// Paint a jagged column of lightning voxels from high in the sky
    /// down to the top of the C. The bolt is two voxels thick at each
    /// segment to make it read at title-screen render scale, and
    /// narrows as it approaches the impact point so it terminates
    /// crisply on the letter.
    fn spawn_bolt(&mut self) {
        let inc_origin = line_origin(LINE_INCIDENT, true);
        let cell_h = FONT_DCP1.cell_height() as u32;
        let scale  = TITLE_SCALE as u32;
        let target_x = self.heli_drop_x as i32;
        // Bolt straddles the C's z-slab on both sides + middle so the
        // cluster reads as a single bright column from the camera's
        // angle rather than a thin sliver.
        let z_mid   = (TITLE_Z + TITLE_DEPTH / 2) as i32;

        let bottom_y = inc_origin.y + cell_h * scale + 2;
        // 48 voxels tall — keeps the whole bolt inside the camera's
        // vertical FOV at the title scene's framing.
        let top_y    = bottom_y + 48;
        let span     = (top_y - bottom_y).max(1) as f32;

        let mut count = 0usize;
        let mut y_center = top_y as i32 - 1; // -1 so the +1 offset doesn't poke above top_y
        while y_center - 1 >= bottom_y as i32 && count + 27 <= self.bolt_cells.len() {
            // Amplitude tapers to ~0 at the impact point so the bolt
            // widens slightly at the sky end and converges crisply on
            // the C. Smaller max amp keeps the bolt readable as one
            // line rather than scattering sub-pixel specks.
            let progress = (top_y as i32 - y_center) as f32 / span; // 0 → 1
            let amp = ((1.0 - progress) * 3.0) as i32 + 1;
            let dx = (self.rng.next_u32() as i32) % (amp * 2 + 1) - amp;
            let x_center = (target_x + dx).max(1);
            // 3×3×3 cluster centred on (x_center, y_center, z_mid).
            // Per-pixel sample density makes 1-voxel-thin features
            // sub-pixel, so every segment is a small solid block.
            for ox in -1..=1i32 {
                for oy in -1..=1i32 {
                    for oz in -1..=1i32 {
                        self.bolt_cells[count] = UVec3::new(
                            (x_center + ox).max(0) as u32,
                            (y_center + oy).max(0) as u32,
                            (z_mid + oz).max(0) as u32,
                        );
                        count += 1;
                    }
                }
            }
            y_center -= 3;
        }
        for i in 0..count {
            set_voxel(self.bolt_cells[i], M_TITLE_LIGHTNING);
        }
        self.bolt_count = count as u16;
        crate::audio::play_thunder();
    }

    /// Clear bolt voxels back to air. Skips any cell that's been
    /// overwritten since (defensive; the bolt sits in empty sky).
    fn clear_bolt(&mut self) {
        for i in 0..self.bolt_count as usize {
            let c = self.bolt_cells[i];
            if material_at(c.x, c.y, c.z) == M_TITLE_LIGHTNING {
                set_voxel(c, 0);
            }
        }
        self.bolt_count = 0;
    }

    fn spawn_heli(&mut self) {
        let actor = actor_spawn().expect("title heli actor");
        self.heli_actor = Some(actor);
        paint_heli_body(actor);
        self.tick_rotor_visual(actor);
        actor_set_position(
            actor,
            Vec3::new(self.heli_x - HELI_SX as f32 * 0.5, HELI_FLY_Y, TITLE_Z as f32 - HELI_SZ as f32 * 0.5),
        );
    }

    fn spawn_menu(&mut self) {
        unsafe {
            prefab_define(
                MENU_PREFAB,
                &*(&raw const MENU_DENSE),
                U8Vec3::new(MENU_W as u8, MENU_H as u8, 1),
            );
        }
        let story   = actor_spawn_from(MENU_PREFAB, Orientation::Up).expect("menu story actor");
        let endless = actor_spawn_from(MENU_PREFAB, Orientation::Up).expect("menu endless actor");
        actor_set_render_mode(story,   ActorRenderMode::Screen);
        actor_set_render_mode(endless, ActorRenderMode::Screen);
        actor_set_position(story,   Vec3::new(MENU_X, MENU_STORY_Y,   100.0));
        actor_set_position(endless, Vec3::new(MENU_X, MENU_ENDLESS_Y, 100.0));
        // Hidden until MenuReady — the user shouldn't see the menu
        // float in over the burning text.
        actor_set_visible(story,   false);
        actor_set_visible(endless, false);
        self.menu_story   = Some(story);
        self.menu_endless = Some(endless);
    }

    /// Advance one frame. `nav_up` / `nav_down` are W / S edges
    /// (true the frame the key crossed NAV_THRESHOLD). `confirm` is
    /// the J edge.
    pub(crate) fn tick(
        &mut self,
        dt_ms: u32,
        nav_up: bool,
        nav_down: bool,
        confirm: bool,
    ) {
        self.phase_ms = self.phase_ms.saturating_add(dt_ms);
        self.total_ms = self.total_ms.saturating_add(dt_ms);

        // Rotor animates throughout — keep the heli looking alive even
        // while idling off-screen.
        self.rotor_phase = self.rotor_phase.wrapping_add(1);
        if let Some(actor) = self.heli_actor {
            self.tick_rotor_visual(actor);
        }

        // Embers + scorch are active while the C is actually burning.
        // We let them run during Approaching (the heli is still on its
        // way) but stop spawning once the drop begins — existing
        // embers age out naturally during the drop. Gated on at least
        // one lit flame so the early frames of Burning (before any
        // cell has been revealed) don't already have embers flying.
        let burning = matches!(self.phase, Phase::Burning | Phase::Approaching)
            && self.fire_count > 0;
        self.step_embers();
        if burning {
            while self.total_ms.saturating_sub(self.last_ember_ms) >= EMBER_SPAWN_MS {
                self.spawn_ember();
                self.last_ember_ms = self.last_ember_ms.saturating_add(EMBER_SPAWN_MS);
            }
            while self.total_ms.saturating_sub(self.last_scorch_ms) >= SCORCH_INTERVAL_MS {
                self.scorch_one();
                self.last_scorch_ms = self.last_scorch_ms.saturating_add(SCORCH_INTERVAL_MS);
            }
        }

        match self.phase {
            Phase::Splash => {
                // Wait for any input edge (nav or confirm). On
                // engagement we kick off the music + strike the bolt
                // so the heli arrival, the fire spreading, and the
                // theme music all land on the same gesture.
                if nav_up || nav_down || confirm {
                    crate::audio::init_title_music();
                    if let Some(a) = self.splash_actor.take() {
                        actor_despawn(a);
                    }
                    // Reset cumulative timers so ember/scorch cadence
                    // counts from the start of Burning, not from page
                    // load — otherwise the first Burning frame would
                    // fire dozens of spawns at once.
                    self.total_ms = 0;
                    self.last_ember_ms = 0;
                    self.last_scorch_ms = 0;
                    self.spawn_bolt();
                    self.transition(Phase::Lightning);
                }
            }
            Phase::Lightning => {
                // Hold the bolt voxels for FLASH_MS, then clear them
                // and wait a beat before the fire takes hold.
                if self.bolt_count > 0 && self.phase_ms >= LIGHTNING_FLASH_MS {
                    self.clear_bolt();
                }
                if self.phase_ms >= LIGHTNING_FLASH_MS + LIGHTNING_HOLD_MS {
                    self.transition(Phase::Burning);
                }
            }
            Phase::Burning => {
                // Reveal seed cells linearly over FIRE_SPREAD_MS so
                // the C visibly catches fire after the strike, then
                // hold for BURNING_HOLD_MS of raging flames before
                // the heli enters.
                let elapsed = self.phase_ms.min(FIRE_SPREAD_MS);
                let target_lit =
                    (self.fire_total as u32 * elapsed / FIRE_SPREAD_MS) as u8;
                while self.fire_count < target_lit {
                    let idx = self.fire_count as usize;
                    Self::fire_cluster_set(self.fire_cells[idx], M_TITLE_FLAME);
                    self.fire_count += 1;
                }
                if self.phase_ms >= FIRE_SPREAD_MS + BURNING_HOLD_MS {
                    // Make sure every seed cell is lit before we hand
                    // off — guards against rounding holes in the ramp.
                    while self.fire_count < self.fire_total {
                        let idx = self.fire_count as usize;
                        Self::fire_cluster_set(self.fire_cells[idx], M_TITLE_FLAME);
                        self.fire_count += 1;
                    }
                    self.transition(Phase::Approaching);
                }
            }
            Phase::Approaching => {
                let dx = HELI_SPEED * (dt_ms as f32 / 1000.0);
                self.heli_x -= dx;
                if self.heli_x <= self.heli_drop_x {
                    self.heli_x = self.heli_drop_x;
                    self.transition(Phase::Dropping);
                }
                self.sync_heli();
            }
            Phase::Dropping => {
                self.drop_water_sheet();
                // Progressively extinguish flames as the drop proceeds.
                // target_remaining(t) = total * (1 - t / D), so the
                // last flame goes out the same frame the phase ends.
                let elapsed = self.phase_ms.min(DROP_DURATION_MS);
                let target_remaining =
                    (self.fire_total as u32 * (DROP_DURATION_MS - elapsed) / DROP_DURATION_MS) as u8;
                while self.fire_count > target_remaining {
                    let idx = self.fire_count as usize - 1;
                    Self::fire_cluster_set(self.fire_cells[idx], 0);
                    self.fire_count -= 1;
                }
                if self.phase_ms >= DROP_DURATION_MS {
                    self.extinguish_all_fire();
                    self.transition(Phase::Departing);
                }
            }
            Phase::Departing => {
                let dx = HELI_SPEED * (dt_ms as f32 / 1000.0);
                self.heli_x -= dx;
                self.sync_heli();
                if self.heli_x <= HELI_END_X && self.phase_ms >= SETTLE_MS {
                    self.transition(Phase::MenuReady);
                    self.reveal_menu();
                }
            }
            Phase::MenuReady => {
                // W → STORY (top option), S → ENDLESS (bottom option).
                // Direct mapping so each key always does the same thing
                // rather than cycling — clearer with only two choices,
                // and matches the vertical layout of the two panels.
                if nav_up   { self.selected = PICK_STORY; }
                if nav_down { self.selected = PICK_ENDLESS; }
                if confirm {
                    self.confirmed = Some(self.selected);
                }
            }
        }

        // Repaint menu when its visual state changes.
        let ready = matches!(self.phase, Phase::MenuReady) as u16;
        let key = (self.selected as u16) | (ready << 8);
        if key != self.menu_cache_key {
            self.menu_cache_key = key;
            self.paint_menu();
        }
    }

    fn transition(&mut self, next: Phase) {
        self.phase = next;
        self.phase_ms = 0;
    }

    fn sync_heli(&self) {
        if let Some(actor) = self.heli_actor {
            actor_set_position(
                actor,
                Vec3::new(
                    self.heli_x - HELI_SX as f32 * 0.5,
                    HELI_FLY_Y + sine(self.rotor_phase as f32 * 0.1) * 0.6,
                    TITLE_Z as f32 - HELI_SZ as f32 * 0.5,
                ),
            );
        }
    }

    fn tick_rotor_visual(&self, actor: ActorId) {
        actor_fill_box(actor, U8Vec3::new(0, 3, 0), U8Vec3::new(4, 3, 4), 0);
        let blade = M_HELICOPTER_ROTOR;
        if self.rotor_phase & 1 == 0 {
            for x in 0u8..5 { actor_set_voxel(actor, U8Vec3::new(x, 3, 2), blade); }
        } else {
            actor_set_voxel(actor, U8Vec3::new(0, 3, 0), blade);
            actor_set_voxel(actor, U8Vec3::new(1, 3, 1), blade);
            actor_set_voxel(actor, U8Vec3::new(2, 3, 2), blade);
            actor_set_voxel(actor, U8Vec3::new(3, 3, 3), blade);
            actor_set_voxel(actor, U8Vec3::new(4, 3, 4), blade);
            actor_set_voxel(actor, U8Vec3::new(0, 3, 4), blade);
            actor_set_voxel(actor, U8Vec3::new(1, 3, 3), blade);
            actor_set_voxel(actor, U8Vec3::new(3, 3, 1), blade);
            actor_set_voxel(actor, U8Vec3::new(4, 3, 0), blade);
        }
    }

    /// Spawn a fresh ember from a random point on top of the burning C.
    fn spawn_ember(&mut self) {
        let slot = match self.embers.iter().position(|e| !e.alive) {
            Some(i) => i,
            None    => return, // pool full; skip
        };

        let inc_origin = line_origin(LINE_INCIDENT, true);
        let cell_h = FONT_DCP1.cell_height() as u32;
        let scale  = TITLE_SCALE as u32;

        let dx = self.rng.range(self.c_world_w) as f32;
        let top_y = (inc_origin.y + cell_h * scale) as f32;
        let z_jit = self.rng.range(TITLE_DEPTH) as f32;
        let pos = Vec3::new(
            self.c_world_x as f32 + dx,
            top_y + 1.0,
            inc_origin.z as f32 + z_jit,
        );
        // Mostly upward; small horizontal jitter so embers fan outward
        // as they rise. Per-tick velocity (called every frame from tick).
        let vx = (self.rng.range(60) as f32 - 30.0) * 0.012;  // ±0.36
        let vy = 0.55 + (self.rng.range(40) as f32) * 0.012;  // 0.55..1.03
        let vz = (self.rng.range(60) as f32 - 30.0) * 0.006;  // ±0.18

        self.embers[slot] = TitleEmber {
            alive: true,
            age:   0,
            last:  UVec3::ZERO,
            pos,
            vel:   Vec3::new(vx, vy, vz),
        };
    }

    /// Move every live ember one tick: clear last paint, advance pos,
    /// repaint at the new cell (if it's air), age out the oldest.
    fn step_embers(&mut self) {
        for e in self.embers.iter_mut() {
            if !e.alive { continue; }

            // Clear the previous paint if we still own that cell. If
            // something else (a flame, water voxel) was put there, leave
            // it alone — we'd rather visually drop the ember than wipe
            // the flame underneath.
            if e.age > 0 && material_at(e.last.x, e.last.y, e.last.z) == M_EMBER {
                set_voxel(e.last, 0);
            }

            e.age = e.age.saturating_add(1);
            if e.age >= EMBER_LIFE_TICKS {
                e.alive = false;
                continue;
            }

            e.pos.x += e.vel.x;
            e.pos.y += e.vel.y;
            e.pos.z += e.vel.z;
            // Slight horizontal damping so embers settle into a vertical
            // plume; upward velocity decays so the tail of an ember's
            // arc curls.
            e.vel.x *= 0.94;
            e.vel.z *= 0.94;
            e.vel.y -= 0.012;

            let cell = UVec3::new(
                e.pos.x.max(0.0) as u32,
                e.pos.y.max(0.0) as u32,
                e.pos.z.max(0.0) as u32,
            );
            // Only paint into air so we don't overwrite the heli, the
            // letters, or another ember.
            if material_at(cell.x, cell.y, cell.z) == 0 {
                set_voxel(cell, M_EMBER);
                e.last = cell;
            }
        }
    }

    /// Char one random body voxel of the C to M_SCORCH. Tries up to a
    /// handful of glyph positions per call — if all rolls land in air
    /// (already-scorched / already-air), we silently skip this round.
    fn scorch_one(&mut self) {
        if (self.scorch_count as usize) >= MAX_SCORCH { return; }

        let inc_origin = line_origin(LINE_INCIDENT, true);
        let cell_h = FONT_DCP1.cell_height() as u32;
        let scale  = TITLE_SCALE as u32;
        let (c_left, _c_right) = FONT_DCP1
            .glyph_ink_bounds('C' as u32)
            .unwrap_or((0, FONT_DCP1.cell_width() - 1));

        for _ in 0..6 {
            // Sample a (col, row) in the C glyph and the corresponding
            // 2×2 scaled block in the world.
            let col_in_ink = self.rng.range(self.c_world_w / scale);
            let col_in_char = (col_in_ink + c_left as u32) as u8;
            let row_in_char = self.rng.range(cell_h) as u8;
            if !FONT_DCP1.glyph_bit('C' as u32, col_in_char, row_in_char) {
                continue;
            }
            let world_x = self.c_world_x + col_in_ink * scale + self.rng.range(scale);
            // Invert row: row 0 = top of glyph in paint_world.
            let world_y = inc_origin.y + (cell_h - 1 - row_in_char as u32) * scale
                + self.rng.range(scale);
            let world_z = inc_origin.z + self.rng.range(TITLE_DEPTH);
            let cell = UVec3::new(world_x, world_y, world_z);
            let m = material_at(cell.x, cell.y, cell.z);
            // Only scorch a painted body voxel (stone/outline) — don't
            // bother re-scorching an already-charred cell or air.
            if m == M_STONE || m == M_HUD_TEXT {
                set_voxel(cell, M_SCORCH);
                self.scorch_count = self.scorch_count.saturating_add(1);
                return;
            }
        }
    }

    /// Paint a narrow column of water voxels falling from the heli
    /// down through the burning "C". The flame extinguish itself is
    /// handled by the progressive clear in `Phase::Dropping`; this is
    /// just the visual curtain.
    fn drop_water_sheet(&mut self) {
        let cx = self.heli_x as i32;
        let cz = TITLE_Z as i32;
        // ±14 keeps the curtain roughly the width of the C (32 vox);
        // a little overhang on each side reads as splash spread.
        let half_w: i32 = 14;
        let top_y: i32 = (HELI_FLY_Y as i32) - 4;
        // INCIDENT's bottom is at TITLE_MID_Y + TITLE_GAP/2 = 224.
        // Bottom the curtain ~14 voxels below that so water visibly
        // splashes past the C without spraying into COMMANDER below.
        let bot_y: i32 = (TITLE_MID_Y as i32) + (TITLE_GAP as i32) / 2 - 14;
        // Every 60 ms we paint 8 water voxels — tight column, dense
        // enough to read as a real downpour on one letter.
        let step_ms = 60;
        if self.phase_ms % step_ms < 16 {
            for _ in 0..8 {
                let dx = (self.rng.next_u32() as i32) % (half_w * 2) - half_w;
                let dz = (self.rng.next_u32() as i32) % 7 - 3;
                let x = (cx + dx).clamp(0, 511) as u32;
                let z = (cz + dz).clamp(0, 511) as u32;
                let y_pick = (bot_y + (self.rng.next_u32() as i32) % (top_y - bot_y).max(1)) as u32;
                set_voxel(UVec3::new(x, y_pick, z), M_WATER);
            }
        }
    }

    fn reveal_menu(&self) {
        if let Some(a) = self.menu_story   { actor_set_visible(a, true); }
        if let Some(a) = self.menu_endless { actor_set_visible(a, true); }
    }

    fn paint_menu(&self) {
        let ready = matches!(self.phase, Phase::MenuReady);
        if let Some(a) = self.menu_story {
            paint_menu_panel(a, "STORY",   ready && self.selected == PICK_STORY);
        }
        if let Some(a) = self.menu_endless {
            paint_menu_panel(a, "ENDLESS", ready && self.selected == PICK_ENDLESS);
        }
    }

    pub(crate) fn confirmed(&self) -> Option<u8> { self.confirmed }

    /// Tear down the title actors before transitioning to gameplay.
    /// Voxels left in the title scene are fine — switching scenes
    /// hides them automatically.
    pub(crate) fn teardown(&mut self) {
        if let Some(a) = self.heli_actor.take()   { actor_despawn(a); }
        if let Some(a) = self.menu_story.take()   { actor_despawn(a); }
        if let Some(a) = self.menu_endless.take() { actor_despawn(a); }
        if let Some(a) = self.splash_actor.take() { actor_despawn(a); }
        crate::audio::stop_title_music();
    }

    /// Push the title's camera. Static framing south-of-letters, slight
    /// pitch down so the letters sit comfortably in frame.
    pub(crate) fn render_camera(&self) {
        let target = Vec3::new(256.0, (TITLE_MID_Y as f32) + 4.0, TITLE_Z as f32);
        // Slow side-to-side sway so the camera doesn't feel completely
        // frozen during the long burning shot.
        let sway = sine(self.phase_ms as f32 / 900.0) * 8.0;
        // Eye sits well *below* the title so most of the framebuffer
        // sees rays going up into the sunset gradient. The renderer's
        // smoothstep heavily weights toward horizon, so we need a lot
        // of upward pitch to see the dusky top of the gradient.
        let eye = Vec3::new(256.0 + sway, target.y - 60.0, TITLE_Z as f32 + 240.0);
        camera_set_lookat(eye, target, Vec3::Y);
        camera_set_fov(40.0);
    }
}

// ── Geometry helpers ──────────────────────────────────────────────

/// Per-line origin (bottom-left of painted volume) in world coords.
/// `is_top` distinguishes the two stacked lines so they don't overlap.
fn line_origin(line: &str, is_top: bool) -> UVec3 {
    // `measure` returns the scaled (kerned) extents the line will
    // actually occupy — match it for centering.
    let m = measure(&FONT_DCP1, TITLE_SCALE, TITLE_DEPTH, line);
    let w = m.x as u32;
    let h = m.y as u32;
    let x = 256u32.saturating_sub(w / 2);
    let z = (TITLE_Z as u32).saturating_sub(TITLE_DEPTH / 2);
    let y = if is_top {
        TITLE_MID_Y + TITLE_GAP / 2
    } else {
        TITLE_MID_Y.saturating_sub(TITLE_GAP / 2 + h)
    };
    UVec3::new(x, y, z)
}

/// World x of a glyph's painted column-0 (after the pen offset of its
/// preceding chars in `line`).
fn glyph_world_x(line: &str, char_idx: usize, line_x: u32) -> u32 {
    let pen = FONT_DCP1.pen_offset(line, char_idx);
    line_x + pen * TITLE_SCALE as u32
}

/// World ink width of a glyph at scale=TITLE_SCALE. Falls back to half
/// a cell for glyphs absent from the font.
fn glyph_world_width(ch: char) -> u32 {
    let scale = TITLE_SCALE as u32;
    match FONT_DCP1.glyph_ink_bounds(ch as u32) {
        Some((l, r)) => (r - l + 1) as u32 * scale,
        None         => (FONT_DCP1.cell_width() / 2).max(2) as u32 * scale,
    }
}

fn paint_title_text() {
    let inc_origin = line_origin(LINE_INCIDENT, true);
    paint_text(
        &FONT_DCP1,
        inc_origin,
        Axis::XY,
        M_STONE,
        Some(M_HUD_TEXT),
        TITLE_SCALE,
        TITLE_DEPTH,
        LINE_INCIDENT,
    );
    let cmd_origin = line_origin(LINE_COMMANDER, false);
    paint_text(
        &FONT_DCP1,
        cmd_origin,
        Axis::XY,
        M_STONE,
        Some(M_HUD_TEXT),
        TITLE_SCALE,
        TITLE_DEPTH,
        LINE_COMMANDER,
    );
}

fn paint_heli_body(actor: ActorId) {
    actor_fill_box(
        actor,
        U8Vec3::new(1, 1, 1),
        U8Vec3::new(3, 2, 3),
        M_HELICOPTER_BODY,
    );
    actor_set_voxel(actor, U8Vec3::new(2, 1, 4), M_HELICOPTER_BODY);
    actor_set_voxel(actor, U8Vec3::new(2, 1, 3), M_HELICOPTER_BODY);
}

// ── Menu painting ─────────────────────────────────────────────────

fn paint_menu_panel(actor: ActorId, label: &str, highlight: bool) {
    actor_clear(actor);
    // Highlighted: paint a dark "badge" — fill the whole panel with
    // near-black, then put cyan border lines on top + bottom so the
    // selection stands as a hard, framed block against the warm
    // sunset sky. Unselected panels stay transparent (text only).
    if highlight {
        for x in 0..MENU_W {
            for y in 0..MENU_H {
                actor_set_voxel(actor, U8Vec3::new(x as u8, y as u8, 0), M_TITLE_MENU_BG);
            }
        }
        for x in 0..MENU_W {
            actor_set_voxel(actor, U8Vec3::new(x as u8, 0, 0), M_TITLE_MENU);
            actor_set_voxel(actor, U8Vec3::new(x as u8, (MENU_H - 1) as u8, 0), M_TITLE_MENU);
        }
    }
    // Center the label horizontally inside the panel.
    let cell_w = FONT_TINY.cell_width() as u32;
    let cell_h = FONT_TINY.cell_height() as u32;
    let text_w = label.chars().count() as u32 * cell_w;
    let left = if text_w >= MENU_W { 0 } else { (MENU_W - text_w) / 2 };
    // Vertically center the (cell_h)-tall glyph in the (MENU_H)-tall
    // panel, anchored from the top-edge of the glyph.
    let top_y = (MENU_H - 1).saturating_sub((MENU_H - cell_h) / 2);
    let color = if highlight { M_TITLE_MENU } else { M_HUD_TEXT };
    let mut col_offset = 0u32;
    for ch in label.chars() {
        for row in 0..cell_h {
            for col in 0..cell_w {
                if !FONT_TINY.glyph_bit(ch as u32, col as u8, row as u8) {
                    continue;
                }
                let x = left + col_offset + col;
                let y = top_y - row;
                if x < MENU_W {
                    actor_set_voxel(actor, U8Vec3::new(x as u8, y as u8, 0), color);
                }
            }
        }
        col_offset += cell_w;
    }
}
