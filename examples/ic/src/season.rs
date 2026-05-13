//! Season state machine. A "season" is the top-level gameplay loop:
//! `DAYS_PER_SEASON` days, each `DAY_DURATION_MS` long, with its own
//! weather and lightning-strike schedule. The player survives the
//! season if at least one cabin is standing at end-of-day-N; the
//! season ends early if every cabin burns down on any day.
//!
//! Story mode (future) is a fixed array of `Season` parameter sets.
//! Endless mode rolls a fresh `Season` from the scenario RNG.

use crate::mathlib::TAU_F32;
use crate::rng::Rng;

pub(crate) const DAYS_PER_SEASON:  u8  = 7;
pub(crate) const DAY_DURATION_MS:  u32 = 120_000;   // 2 minutes per day

/// Soft floor on time between strikes within a day (ms). Without this
/// a "dry storm" day could pile multiple strikes within a second of
/// each other even with the active-incident gate.
const STRIKE_COOLDOWN_MS:          u32 = 8_000;

/// First-strike-of-day windows (per weather profile). Each day rolls a
/// strike at `first_strike_lo..first_strike_hi` ms into the day.
const FIRST_STRIKE_MIN_MS:         u32 = 5_000;
const FIRST_STRIKE_MAX_MS:         u32 = 25_000;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum SeasonState {
    DayActive,
    /// Survived all `DAYS_PER_SEASON` with at least one cabin standing.
    SeasonWon,
    /// Cabins all destroyed at some point during the season.
    SeasonLost,
}

/// Per-day weather profile. Rolled fresh at the start of each day.
#[derive(Copy, Clone)]
pub(crate) struct DailyWeather {
    /// Wind direction (radians, "blowing toward"). Matches fire.rs's
    /// convention: 0 = -Z (north), π/2 = +X (east).
    pub angle_rad: f32,
    /// Wind strength scalar 0..1.
    pub strength: f32,
    /// Strikes per day rolled at day start. Drives the strike rate.
    pub strike_budget: u8,
}

pub(crate) struct Season {
    pub day: u8,
    pub day_time_ms: u32,
    pub state: SeasonState,
    rng: Rng,
    pub weather: DailyWeather,
    /// Ms-in-day for the next scheduled lightning strike. `u32::MAX`
    /// means "no more strikes scheduled today" (budget exhausted).
    next_strike_ms: u32,
    /// Last strike fire-time, used to enforce STRIKE_COOLDOWN_MS.
    last_strike_ms: u32,
    /// Total strikes that have fired this day. Compared against
    /// `weather.strike_budget`.
    pub strikes_today: u8,
    /// Total strikes that have fired this season, across all days.
    pub strikes_total: u32,
}

impl Season {
    pub(crate) fn new(seed: u32) -> Self {
        let mut rng = Rng::new(seed ^ 0xB529_7A4D);
        let mut s = Self {
            day: 0,
            day_time_ms: 0,
            state: SeasonState::DayActive,
            rng,
            weather: DailyWeather {
                angle_rad: 0.0,
                strength: 0.5,
                strike_budget: 1,
            },
            next_strike_ms: 0,
            last_strike_ms: 0,
            strikes_today: 0,
            strikes_total: 0,
        };
        s.roll_day();
        s
    }

    /// One-line label of current weather, fits 8-char HUD column.
    pub(crate) fn weather_glyph(&self) -> &'static str {
        // 4-bucket "intensity" forecast, derived from strike budget +
        // wind strength so the HUD has something the player can read
        // at a glance to anticipate the day.
        let intensity = self.weather.strike_budget as u32
            + (self.weather.strength * 4.0) as u32;
        match intensity {
            0..=1 => "CALM",
            2..=3 => "MILD",
            4..=5 => "GUST",
            _     => "STORM",
        }
    }

    /// Tick the season clock. Returns `Some((x, z))` if a lightning
    /// strike should ignite a fire this frame, otherwise `None`. The
    /// caller must:
    ///   - early-out when `state != DayActive`
    ///   - apply the strike position to the world (terrain::strike)
    ///   - pass `incident_active = true` while any cabin-threatening
    ///     fire / active unit work is in progress, to suppress new
    ///     strikes during the current incident
    pub(crate) fn tick(&mut self, dt_ms: u32, incident_active: bool) -> Option<(u32, u32)> {
        if self.state != SeasonState::DayActive { return None; }

        self.day_time_ms = self.day_time_ms.saturating_add(dt_ms);

        if self.day_time_ms >= DAY_DURATION_MS {
            self.advance_day();
            return None;
        }

        // Lightning gating: incident_active suppresses strikes, and we
        // enforce a hard cooldown floor between strikes too.
        if incident_active { return None; }
        if self.strikes_today >= self.weather.strike_budget { return None; }
        if self.next_strike_ms == u32::MAX { return None; }
        if self.day_time_ms < self.next_strike_ms { return None; }
        if self.day_time_ms < self.last_strike_ms.saturating_add(STRIKE_COOLDOWN_MS) {
            return None;
        }

        // Strike! Pick an XZ inside the dense-forest zone — same
        // bounds the pine scatter uses (terrain.rs scatters tz in
        // ~[36, 154) and tx across most of the footprint), so a
        // strike almost always lands within ember-reach of fuel.
        let x = 40 + (self.rng.next_u32() % 180);
        let z = 40 + (self.rng.next_u32() % 100);

        self.last_strike_ms = self.day_time_ms;
        self.strikes_today += 1;
        self.strikes_total += 1;

        // Schedule next strike if budget remains. Mean inter-strike
        // time = remaining-day / remaining-budget, with ±50% jitter.
        let remaining_budget = self.weather.strike_budget - self.strikes_today;
        if remaining_budget == 0 {
            self.next_strike_ms = u32::MAX;
        } else {
            let day_left = DAY_DURATION_MS.saturating_sub(self.day_time_ms);
            let mean = day_left / remaining_budget as u32;
            let jitter = mean / 2;
            let offset = (self.rng.next_u32() % jitter.max(1)).saturating_sub(jitter / 2);
            self.next_strike_ms = self.day_time_ms.saturating_add(mean).saturating_add(offset);
        }

        Some((x, z))
    }

    /// End the season early — called when the cabin count drops to 0.
    pub(crate) fn end_lost(&mut self) {
        self.state = SeasonState::SeasonLost;
    }

    fn advance_day(&mut self) {
        if self.day + 1 >= DAYS_PER_SEASON {
            self.state = SeasonState::SeasonWon;
            return;
        }
        self.day += 1;
        self.day_time_ms = 0;
        self.last_strike_ms = 0;
        self.strikes_today = 0;
        self.roll_day();
    }

    /// Roll a fresh weather profile + first-strike timer for the new
    /// day. Called from `new()` and `advance_day()`.
    fn roll_day(&mut self) {
        // Wind angle full 360°. Most days the wind will randomly miss
        // the town and embers go off-map; the few days it points at
        // the town will produce the bulk of action.
        self.weather.angle_rad = self.rng.unit() * TAU_F32;
        self.weather.strength = 0.20 + self.rng.unit() * 0.70;
        // Strike budget rolls 1..=4 per day, with later-season days
        // skewed slightly hotter so the season ramps up.
        let day_bias = (self.day as f32 / DAYS_PER_SEASON as f32) * 0.5;
        let roll = self.rng.unit() + day_bias;
        self.weather.strike_budget = if      roll < 0.30 { 1 }
                                     else if roll < 0.65 { 2 }
                                     else if roll < 0.90 { 3 }
                                     else                { 4 };

        // First strike fires somewhere in the warm-up window.
        let first = FIRST_STRIKE_MIN_MS
            + (self.rng.next_u32() % (FIRST_STRIKE_MAX_MS - FIRST_STRIKE_MIN_MS));
        self.next_strike_ms = first;
    }
}
