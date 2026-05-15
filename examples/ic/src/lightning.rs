//! In-game lightning strike effect. When `season.rs` rolls a new
//! strike the cart calls [`Lightning::strike`], which:
//!
//!   1. picks an ignition target via [`terrain::find_strike_target`],
//!   2. rolls a "dud" chance — even with a valid target, some strikes
//!      deliberately fail to ignite (player has to read the sky and
//!      hope the bolt didn't catch),
//!   3. paints a jagged column of bright `M_TITLE_LIGHTNING` voxels
//!      from high above the impact point down to the target cell, and
//!   4. plays the thunder SFX.
//!
//! After [`FLASH_MS`] the bolt voxels are cleared back to air; if the
//! strike wasn't a dud the impact cell becomes `M_FIRE` and the cart
//! adds it to `FireState`'s burn-site list.
//!
//! The bolt material slot is shared with the title-screen strike
//! (`M_TITLE_LIGHTNING`) — same yellow:3 / emission 15 lookup, no new
//! material needed.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics::material_at;

use crate::audio;
use crate::rng::Rng;
use crate::terrain::{find_strike_target, terrain_height};
use crate::{M_FIRE, M_TITLE_LIGHTNING};

/// Bolt voxel cap. Each segment paints a 3×3×3 cluster (27 voxels);
/// the bolt is ~BOLT_HEIGHT voxels tall stepped every 3 voxels, so a
/// full column uses ~14 × 27 ≈ 380 cells. 512 leaves headroom for
/// the extra cluster at the top and any rounding.
const BOLT_CAP: usize = 512;

/// How long the bolt voxels stay painted before the cart clears them
/// and (if applicable) ignites the target. Short enough to read as a
/// flash, long enough that the player's eye catches the bright column.
const FLASH_MS: u32 = 350;

/// Per-strike chance to roll a "dud" even when a flammable target
/// was found. Adds a beat of suspense — every flash is real, but not
/// every flash starts a fire.
const DUD_CHANCE: f32 = 0.25;

/// Sky height of the bolt above its impact cell, in voxels. The RTS
/// camera tilts ~54° down from above, so a tall vertical column
/// quickly disappears off the top of the viewport. 12 keeps the
/// whole bolt visible across the zoom range and still reads as
/// "sky-to-canopy" at the cart's scale.
const BOLT_HEIGHT: i32 = 12;

pub(crate) struct Lightning {
    /// Painted bolt cells with the material that was there *before*
    /// the bolt overwrote it, so we can restore the original on flash
    /// end. The bolt routinely passes through pine canopy at the
    /// impact point; without this, clearing the bolt would erase the
    /// pine wherever the bolt overlapped.
    bolt_cells: [(UVec3, u8); BOLT_CAP],
    bolt_count: u16,
    /// Ms remaining in the current flash. 0 = idle.
    flash_remaining: u32,
    /// Cell to ignite when the flash ends + its pre-ignition material
    /// (drives the per-material burn TTL in `fire.rs`). None on duds.
    pending_ignite: Option<(UVec3, u8)>,
    rng: Rng,
}

impl Lightning {
    pub(crate) const fn new() -> Self {
        Self {
            bolt_cells: [(UVec3::ZERO, 0); BOLT_CAP],
            bolt_count: 0,
            flash_remaining: 0,
            pending_ignite: None,
            rng: Rng(0xCAFEBABE),
        }
    }

    pub(crate) fn init(&mut self, seed: u32) {
        self.rng = Rng::new(seed ^ 0x4C39_77B1);
        self.bolt_count = 0;
        self.flash_remaining = 0;
        self.pending_ignite = None;
    }

    /// Returns true while the current flash is still on-screen.
    pub(crate) fn flashing(&self) -> bool { self.flash_remaining > 0 }

    /// Trigger a strike aimed at `(sx, sz)`. Decides ignition target +
    /// dud, paints the bolt, plays thunder. Returns `(cell, material)`
    /// that *will* ignite when the flash ends, or `None` if this
    /// strike is a dud. `material` is the pre-ignition slot at the
    /// target cell — `fire.rs` consumes it to pick a per-material
    /// burn TTL.
    pub(crate) fn strike(&mut self, sx: u32, sz: u32) -> Option<(UVec3, u8)> {
        // Defensive: clear any in-flight bolt before reusing the
        // buffer. STRIKE_COOLDOWN_MS in season.rs is much longer than
        // FLASH_MS so we shouldn't actually overlap, but if we ever
        // do, leaving the old voxels around would corrupt the world.
        if self.flash_remaining > 0 { self.clear_bolt(); }

        let target = find_strike_target(sx, sz);
        let is_dud = target.is_none() || self.rng.unit() < DUD_CHANCE;
        self.pending_ignite = if is_dud { None } else { target };

        // Land the bolt on the actual ignition cell when we have one
        // so the flash converges on where the fire will appear; for
        // duds, fall back to the targeted XZ at terrain height so the
        // bolt still hits the ground visibly.
        let (impact_x, impact_y, impact_z) = match target {
            Some((p, _)) => (p.x as i32, p.y as i32, p.z as i32),
            None => {
                let h = terrain_height(sx, sz);
                (sx as i32, (h + 1) as i32, sz as i32)
            }
        };

        self.spawn_bolt(impact_x, impact_y, impact_z);
        audio::play_thunder();
        self.flash_remaining = FLASH_MS;
        self.pending_ignite
    }

    /// Advance the flash timer. Returns `(cell, material)` to ignite
    /// the frame the flash ends (caller adds it as a burn site with
    /// the matching per-material TTL), else `None`.
    pub(crate) fn tick(&mut self, dt_ms: u32) -> Option<(UVec3, u8)> {
        if self.flash_remaining == 0 { return None; }
        if dt_ms < self.flash_remaining {
            self.flash_remaining -= dt_ms;
            return None;
        }
        self.flash_remaining = 0;
        self.clear_bolt();
        if let Some((cell, material)) = self.pending_ignite.take() {
            set_voxel(cell, M_FIRE);
            Some((cell, material))
        } else {
            None
        }
    }

    /// Paint a jagged 3×3×3-cluster column of bright lightning voxels
    /// from `BOLT_HEIGHT` above the impact cell down to it. Mirror of
    /// the title-screen bolt, adapted to any XYZ anchor.
    fn spawn_bolt(&mut self, impact_x: i32, impact_y: i32, impact_z: i32) {
        let top_y = impact_y + BOLT_HEIGHT;
        let span = (top_y - impact_y).max(1) as f32;
        let mut count = 0usize;
        let mut y_center = top_y - 1;
        while y_center - 1 >= impact_y && count + 27 <= self.bolt_cells.len() {
            // Amplitude tapers from ~3 at the sky end to 0 at impact
            // so the bolt converges crisply on the target instead of
            // smearing across the canopy.
            let progress = (top_y - y_center) as f32 / span;
            let amp = ((1.0 - progress) * 3.0) as i32 + 1;
            let span_x = amp * 2 + 1;
            let span_z = amp * 2 + 1;
            let dx = (self.rng.next_u32() % span_x as u32) as i32 - amp;
            let dz = (self.rng.next_u32() % span_z as u32) as i32 - amp;
            let x_center = (impact_x + dx).max(0);
            let z_center = (impact_z + dz).max(0);
            for ox in -1..=1i32 {
                for oy in -1..=1i32 {
                    for oz in -1..=1i32 {
                        if count >= self.bolt_cells.len() { break; }
                        let cell = UVec3::new(
                            (x_center + ox).max(0) as u32,
                            (y_center + oy).max(0) as u32,
                            (z_center + oz).max(0) as u32,
                        );
                        // Capture the original material before
                        // overwriting so clear_bolt can put it back.
                        let original = material_at(cell.x, cell.y, cell.z);
                        self.bolt_cells[count] = (cell, original);
                        count += 1;
                    }
                }
            }
            y_center -= 3;
        }
        for i in 0..count {
            set_voxel(self.bolt_cells[i].0, M_TITLE_LIGHTNING);
        }
        self.bolt_count = count as u16;
    }

    fn clear_bolt(&mut self) {
        for i in 0..self.bolt_count as usize {
            let (cell, original) = self.bolt_cells[i];
            // Only restore if the bolt voxel is still ours — another
            // system (e.g. fire spread) may have written here in the
            // meantime, and overwriting would clobber that.
            if material_at(cell.x, cell.y, cell.z) == M_TITLE_LIGHTNING {
                set_voxel(cell, original);
            }
        }
        self.bolt_count = 0;
    }
}
