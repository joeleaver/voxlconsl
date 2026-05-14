//! Story-mode level table. A "level" is a preset Season with a fixed
//! seed + tier + length. Endless mode rolls a fresh procedural Season
//! from `MISSION_SEED`; Story mode steps through this table in order.
//!
//! This is scaffolding — only the data shape is in place. The cart's
//! `STORY_LEVEL` const in `lib.rs` picks which entry to load; there's
//! no in-game level selector yet. Future work: title screen with a
//! grid of unlocked levels and a save-block-backed progression flag.

#[allow(dead_code)] // some fields aren't wired into Season yet.
pub(crate) struct StoryLevel {
    /// Short display name (8 chars max so it fits a single
    /// FONT_TINY-on-32×32 line later).
    pub name: &'static str,
    pub seed: u32,
    pub tier: u8,
    /// Days in this season. Override of the default
    /// `season::DAYS_PER_SEASON` (=7) so levels can be shorter or
    /// longer than the procedural default.
    pub days: u8,
}

pub(crate) const LEVELS: &[StoryLevel] = &[
    StoryLevel {
        name: "TUTORIAL",
        seed: 0x10_00_00_01,
        tier: 1,
        days: 3,
    },
    StoryLevel {
        name: "FOOTHILLS",
        seed: 0x10_00_00_02,
        tier: 2,
        days: 5,
    },
    StoryLevel {
        name: "DROUGHT",
        seed: 0x10_00_00_03,
        tier: 3,
        days: 7,
    },
];
