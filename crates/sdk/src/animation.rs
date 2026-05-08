//! Cart-side animation helpers — see SPEC.md §11.9.
//!
//! v1 is **flipbook** only: animations are sequences of prefab IDs swapped
//! over time. Skeletal / parted animation is a v2 stretch.
//!
//! The helpers in this module are pure cart-side logic. The host knows
//! nothing about animation clips — it just receives `actor_set_prefab`
//! calls. This means animation behavior (timing, looping, blending,
//! frame-callback semantics) is entirely cart-controlled and can evolve
//! without host changes.

use voxlconsl_types::PrefabId;

/// A timed prefab-cycling animation clip.
///
/// Constructed once (often as a `static mut`), ticked once per frame from
/// `update`, and used to source the current `PrefabId` for one or more
/// actors. See SPEC.md §11.9 for usage examples.
pub struct Flipbook {
    frames: &'static [PrefabId],
    frame_duration_ms: u32,
    looping: bool,
    elapsed_ms: u32,
    current_idx: u32,
    advanced_this_tick: bool,
    completed: bool,
}

impl Flipbook {
    /// Build a clip from a list of prefab IDs and a uniform per-frame duration.
    ///
    /// `looping = true` cycles back to frame 0 after the last frame.
    /// `looping = false` ends on the last frame and reports `is_done()` true.
    pub const fn new(
        frames: &'static [PrefabId],
        frame_duration_ms: u32,
        looping: bool,
    ) -> Self {
        Self {
            frames,
            frame_duration_ms,
            looping,
            elapsed_ms: 0,
            current_idx: 0,
            advanced_this_tick: false,
            completed: false,
        }
    }

    /// Advance the playhead by `dt_ms`. Should be called once per frame
    /// from cart `update`.
    pub fn tick(&mut self, dt_ms: u32) {
        if self.completed || self.frames.is_empty() {
            self.advanced_this_tick = false;
            return;
        }

        self.elapsed_ms = self.elapsed_ms.saturating_add(dt_ms);
        let mut advanced = false;

        while self.elapsed_ms >= self.frame_duration_ms {
            self.elapsed_ms -= self.frame_duration_ms;
            self.current_idx += 1;
            advanced = true;

            if self.current_idx >= self.frames.len() as u32 {
                if self.looping {
                    self.current_idx = 0;
                } else {
                    self.current_idx = (self.frames.len() - 1) as u32;
                    self.completed = true;
                    break;
                }
            }
        }

        self.advanced_this_tick = advanced;
    }

    /// The prefab ID of the current frame. Pass to `actor_set_prefab`.
    pub fn current(&self) -> PrefabId {
        self.frames[self.current_idx as usize]
    }

    /// Index of the current frame in the clip's frame list.
    pub fn current_frame(&self) -> usize {
        self.current_idx as usize
    }

    /// Number of frames in the clip.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Reset the playhead to frame 0 and clear completed state.
    pub fn reset(&mut self) {
        self.elapsed_ms = 0;
        self.current_idx = 0;
        self.advanced_this_tick = false;
        self.completed = false;
    }

    /// True for non-looping clips that have reached the last frame.
    /// Always false for looping clips.
    pub fn is_done(&self) -> bool {
        self.completed
    }

    /// Edge: true on the tick the playhead just landed on `frame`.
    ///
    /// Useful for triggering frame-synced events (footstep SFX, hit-frame
    /// damage application, etc.):
    ///
    /// ```ignore
    /// walk.tick(dt_ms);
    /// if walk.just_entered_frame(0) { sfx_play(FOOTSTEP_LEFT, ...); }
    /// if walk.just_entered_frame(2) { sfx_play(FOOTSTEP_RIGHT, ...); }
    /// ```
    ///
    /// Returns false if `frame` is out of range.
    pub fn just_entered_frame(&self, frame: usize) -> bool {
        self.advanced_this_tick && self.current_idx as usize == frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAMES: &[PrefabId] = &[PrefabId(10), PrefabId(11), PrefabId(12), PrefabId(13)];

    #[test]
    fn looping_advances_through_frames() {
        let mut fb = Flipbook::new(FRAMES, 100, true);
        assert_eq!(fb.current_frame(), 0);

        fb.tick(50);
        assert_eq!(fb.current_frame(), 0);
        assert!(!fb.just_entered_frame(0));

        fb.tick(60);
        assert_eq!(fb.current_frame(), 1);
        assert!(fb.just_entered_frame(1));

        fb.tick(50);
        assert_eq!(fb.current_frame(), 1);
        assert!(!fb.just_entered_frame(1));

        // Walk through the rest and confirm wrap-around.
        fb.tick(100);
        assert_eq!(fb.current_frame(), 2);
        fb.tick(100);
        assert_eq!(fb.current_frame(), 3);
        fb.tick(100);
        assert_eq!(fb.current_frame(), 0);
        assert!(!fb.is_done());
    }

    #[test]
    fn one_shot_clip_completes() {
        let mut fb = Flipbook::new(FRAMES, 100, false);
        // Big tick that should run past the end.
        fb.tick(1000);
        assert_eq!(fb.current_frame(), FRAMES.len() - 1);
        assert!(fb.is_done());

        // Subsequent ticks no-op once completed.
        fb.tick(500);
        assert_eq!(fb.current_frame(), FRAMES.len() - 1);
        assert!(!fb.advanced_this_tick);
    }

    #[test]
    fn reset_returns_to_initial_state() {
        let mut fb = Flipbook::new(FRAMES, 100, false);
        fb.tick(1000);
        assert!(fb.is_done());
        fb.reset();
        assert!(!fb.is_done());
        assert_eq!(fb.current_frame(), 0);
    }

    #[test]
    fn just_entered_fires_only_on_transition() {
        let mut fb = Flipbook::new(FRAMES, 100, true);
        fb.tick(100);   // → frame 1
        assert!(fb.just_entered_frame(1));

        fb.tick(50);    // still on frame 1, didn't transition
        assert!(!fb.just_entered_frame(1));

        // Multiple-frame jump in a single tick: still records the latest landing.
        fb.tick(250);   // 1 → 2 → 3 → 4 (which wraps to frame 0 on looping)
        assert_eq!(fb.current_frame(), 0);
        assert!(fb.just_entered_frame(0));
    }
}
