//! Input — see SPEC.md §6.
//!
//! v0.0.3: action declaration + Button polling (held / pressed / released /
//! held_ms) + Axis2D polling. Auto-binds declared actions to a fixed
//! browser-port key-and-mouse set per the §6.6 default tables.
//!
//! TODO:
//!   - Reserved system actions (§6.3)
//!   - Per-port binding tables for handheld + touch
//!   - Rebind UI (§6.8)
//!   - Stick deadzone tuning (§6.6)
//!   - Rumble output (§6.9)

use std::collections::HashSet;

use voxlconsl_types::{ActionHandle, ActionKind, BindingHint};

/// Numeric IDs for the keys the browser port recognizes. Browser-side JS
/// maps `KeyboardEvent.code` strings to these IDs before calling into the
/// host. Order doesn't matter; just keep it stable.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    W = 0, A = 1, S = 2, D = 3,
    I = 4, J = 5, K = 6, L = 7,
    Q = 8, E = 9,
    Space = 10, Enter = 11, Tab = 12, Escape = 13,
    Shift = 14, RShift = 15,
    Up = 16, Down = 17, Left = 18, Right = 19,
    F1 = 20,
    U  = 21,
}

impl Key {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::W), 1 => Some(Self::A), 2 => Some(Self::S), 3 => Some(Self::D),
            4 => Some(Self::I), 5 => Some(Self::J), 6 => Some(Self::K), 7 => Some(Self::L),
            8 => Some(Self::Q), 9 => Some(Self::E),
            10 => Some(Self::Space), 11 => Some(Self::Enter),
            12 => Some(Self::Tab), 13 => Some(Self::Escape),
            14 => Some(Self::Shift), 15 => Some(Self::RShift),
            16 => Some(Self::Up), 17 => Some(Self::Down),
            18 => Some(Self::Left), 19 => Some(Self::Right),
            20 => Some(Self::F1),
            21 => Some(Self::U),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RegisteredAction {
    pub name: String,
    pub kind: ActionKind,
    pub hint: BindingHint,
    /// Resolved binding (browser port). Populated at declaration time.
    pub binding: Binding,
}

#[derive(Clone, Debug)]
pub enum Binding {
    /// Two keys produce -1 / +1 along a single axis; pair gives 2D.
    Axis2DKeyPair {
        neg_x: Key, pos_x: Key,
        neg_y: Key, pos_y: Key,
    },
    /// Two keys produce 0/1 along a single axis (pos-only).
    Axis1DKey { pos: Key },
    /// Single key acts as a button.
    Button { key: Key },
    /// Mouse delta accumulated since last frame.
    MouseDelta,
    /// Mouse wheel delta accumulated since last frame. Positive = zoom in
    /// (browser wheel scrolls up = positive value), negative = zoom out.
    MouseWheel,
    /// Unbound — no physical input maps to this action on this port.
    None,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct ButtonSnapshot {
    pub held: bool,
    pub pressed: bool,
    pub released: bool,
    pub held_ms: u32,
}

pub struct InputState {
    actions: Vec<RegisteredAction>,
    keys_held: HashSet<Key>,
    keys_pressed_this_frame: HashSet<Key>,
    keys_released_this_frame: HashSet<Key>,
    /// Per-key millisecond counters since press. Updated each frame.
    keys_held_ms: std::collections::HashMap<Key, u32>,
    /// Mouse delta accumulated since the last frame boundary.
    mouse_dx: f32,
    mouse_dy: f32,
    /// Wheel delta accumulated since the last frame boundary. Positive
    /// = zoom in (wheel scrolled up), negative = zoom out.
    wheel_dy: f32,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            keys_held: HashSet::new(),
            keys_pressed_this_frame: HashSet::new(),
            keys_released_this_frame: HashSet::new(),
            keys_held_ms: std::collections::HashMap::new(),
            mouse_dx: 0.0,
            mouse_dy: 0.0,
            wheel_dy: 0.0,
        }
    }

    /// Register a new action. Returns the assigned handle.
    pub fn declare(
        &mut self,
        name: String,
        kind: ActionKind,
        hint: BindingHint,
    ) -> ActionHandle {
        let binding = browser_default_binding(kind, hint);
        self.actions.push(RegisteredAction { name, kind, hint, binding });
        ActionHandle((self.actions.len() - 1) as u32)
    }

    pub fn get(&self, h: ActionHandle) -> Option<&RegisteredAction> {
        self.actions.get(h.0 as usize)
    }

    pub fn is_active(&self, h: ActionHandle) -> bool {
        self.get(h).map(|a| !matches!(a.binding, Binding::None)).unwrap_or(false)
    }

    /// Short human-readable label for the input currently bound to `h`.
    /// See SPEC §6.5. Returns `""` when the handle is unbound on this
    /// port or refers to an unknown action.
    ///
    /// The browser port only ever binds keyboard + mouse, so the labels
    /// are stable strings keyed off `Binding`. When gamepad / handheld
    /// bindings land this needs to grow a case per device class.
    pub fn label(&self, h: ActionHandle) -> &'static str {
        // Reserved system handles aren't in `self.actions` — synthesise
        // the label from the browser default for the corresponding key.
        if h == ActionHandle::SYSTEM_PAUSE { return key_label(Key::Tab); }
        if h == ActionHandle::SYSTEM_MENU  { return key_label(Key::F1); }
        match self.get(h).map(|a| &a.binding) {
            Some(Binding::Button { key }) => key_label(*key),
            Some(Binding::Axis1DKey { pos }) => key_label(*pos),
            Some(Binding::Axis2DKeyPair { neg_x, pos_x, neg_y, pos_y }) => {
                // Recognise the canonical layouts so the cart can paint
                // "WASD" / "Arrows" instead of leaking individual keys.
                match (*neg_x, *pos_x, *neg_y, *pos_y) {
                    (Key::A, Key::D, Key::S, Key::W) => "WASD",
                    (Key::Left, Key::Right, Key::Down, Key::Up) => "Arrows",
                    _ => "Keys",
                }
            }
            Some(Binding::MouseDelta) => "Mouse",
            Some(Binding::MouseWheel) => "Wheel",
            Some(Binding::None) | None => "",
        }
    }

    // ── Button-style queries ────────────────────────────────────────────

    pub fn button(&self, h: ActionHandle) -> bool {
        match self.get(h).map(|a| &a.binding) {
            Some(Binding::Button { key }) => self.keys_held.contains(key),
            _ => false,
        }
    }

    pub fn button_pressed(&self, h: ActionHandle) -> bool {
        match self.get(h).map(|a| &a.binding) {
            Some(Binding::Button { key }) => self.keys_pressed_this_frame.contains(key),
            _ => false,
        }
    }

    pub fn button_released(&self, h: ActionHandle) -> bool {
        match self.get(h).map(|a| &a.binding) {
            Some(Binding::Button { key }) => self.keys_released_this_frame.contains(key),
            _ => false,
        }
    }

    pub fn button_held_ms(&self, h: ActionHandle) -> u32 {
        match self.get(h).map(|a| &a.binding) {
            Some(Binding::Button { key }) => self.keys_held_ms.get(key).copied().unwrap_or(0),
            _ => 0,
        }
    }

    // ── Axis queries ────────────────────────────────────────────────────

    pub fn axis1d(&self, h: ActionHandle) -> f32 {
        match self.get(h).map(|a| &a.binding) {
            Some(Binding::Axis1DKey { pos }) => {
                if self.keys_held.contains(pos) { 1.0 } else { 0.0 }
            }
            Some(Binding::MouseWheel) => self.wheel_dy,
            _ => 0.0,
        }
    }

    pub fn axis2d(&self, h: ActionHandle) -> (f32, f32) {
        match self.get(h).map(|a| &a.binding) {
            Some(Binding::Axis2DKeyPair { neg_x, pos_x, neg_y, pos_y }) => {
                let mut x = 0.0;
                let mut y = 0.0;
                if self.keys_held.contains(pos_x) { x += 1.0; }
                if self.keys_held.contains(neg_x) { x -= 1.0; }
                if self.keys_held.contains(pos_y) { y += 1.0; }
                if self.keys_held.contains(neg_y) { y -= 1.0; }
                (x, y)
            }
            Some(Binding::MouseDelta) => (self.mouse_dx, self.mouse_dy),
            _ => (0.0, 0.0),
        }
    }

    // ── Browser-side input plumbing ─────────────────────────────────────

    /// Called by the browser host whenever a tracked key changes state.
    /// `down` = true is a press, false is a release.
    ///
    /// The press edge fires unconditionally so we recover gracefully
    /// from a missed `keyup` — a window-blur or alt-tab can swallow
    /// the release, leaving the key in `keys_held` forever. Without
    /// the unconditional insert, the next fresh press would see the
    /// key "already held" and silently drop the edge, making the
    /// input feel flaky. Browser auto-repeat is filtered out on the
    /// JS side (`e.repeat`) so this only fires once per real tap.
    pub fn key_event(&mut self, key: Key, down: bool) {
        if down {
            self.keys_pressed_this_frame.insert(key);
            self.keys_held.insert(key);
            self.keys_held_ms.insert(key, 0);
        } else if self.keys_held.remove(&key) {
            self.keys_released_this_frame.insert(key);
            self.keys_held_ms.remove(&key);
        }
    }

    /// Accumulate mouse motion since the last frame boundary.
    pub fn add_mouse_delta(&mut self, dx: f32, dy: f32) {
        self.mouse_dx += dx;
        self.mouse_dy += dy;
    }

    /// Accumulate wheel motion since the last frame boundary. Browser
    /// hosts pass `-event.deltaY / 100` so one wheel notch ≈ ±1.0,
    /// positive = zoom in.
    pub fn add_wheel_delta(&mut self, dy: f32) {
        self.wheel_dy += dy;
    }

    /// Called by the runtime once per frame after `cart.update` finishes,
    /// to clear edge-triggered events and roll forward held-time counters.
    pub fn end_of_frame(&mut self, dt_ms: u32) {
        self.keys_pressed_this_frame.clear();
        self.keys_released_this_frame.clear();
        for ms in self.keys_held_ms.values_mut() {
            *ms = ms.saturating_add(dt_ms);
        }
        // Mouse + wheel deltas are consumed each frame.
        self.mouse_dx = 0.0;
        self.mouse_dy = 0.0;
        self.wheel_dy = 0.0;
    }
}

impl Default for InputState {
    fn default() -> Self { Self::new() }
}

/// Short display name for a browser keyboard key — see SPEC §6.5. Stable
/// strings the cart prints into its HUD as "press [Esc] to cancel" etc.
fn key_label(k: Key) -> &'static str {
    match k {
        Key::W => "W", Key::A => "A", Key::S => "S", Key::D => "D",
        Key::I => "I", Key::J => "J", Key::K => "K", Key::L => "L",
        Key::Q => "Q", Key::E => "E", Key::U => "U",
        Key::Space => "Space", Key::Enter => "Enter",
        Key::Tab => "Tab", Key::Escape => "Esc",
        Key::Shift => "Shift", Key::RShift => "RShift",
        Key::Up => "Up", Key::Down => "Down",
        Key::Left => "Left", Key::Right => "Right",
        Key::F1 => "F1",
    }
}

/// Default browser-port bindings (subset of SPEC.md §6.7, no-gamepad column).
fn browser_default_binding(kind: ActionKind, hint: BindingHint) -> Binding {
    use BindingHint::*;
    match (kind, hint) {
        (ActionKind::Axis2D, PrimaryMovement) => Binding::Axis2DKeyPair {
            neg_x: Key::A, pos_x: Key::D,
            // Y up = forward
            neg_y: Key::S, pos_y: Key::W,
        },
        (ActionKind::Axis2D, Aim) => Binding::MouseDelta,
        (ActionKind::Axis2D, _) => Binding::Axis2DKeyPair {
            neg_x: Key::Left, pos_x: Key::Right,
            neg_y: Key::Down, pos_y: Key::Up,
        },
        // Face-button surface for the browser port maps to the J/K/U/I
        // diamond under the right hand. System buttons land on
        // Enter/Escape since they're the most-reached-for "go" / "back"
        // keys when right hand is on the surface row.
        (ActionKind::Button, PrimaryFire) => Binding::Button { key: Key::J },
        (ActionKind::Button, SecondaryFire) => Binding::Button { key: Key::K },
        (ActionKind::Button, Confirm) => Binding::Button { key: Key::U },
        (ActionKind::Button, Cancel) => Binding::Button { key: Key::I },
        (ActionKind::Button, Pause) => Binding::Button { key: Key::Enter },
        (ActionKind::Button, Menu) => Binding::Button { key: Key::Escape },
        (ActionKind::Button, _) => Binding::Button { key: Key::Space },
        (ActionKind::Axis1D, Zoom) => Binding::MouseWheel,
        (ActionKind::Axis1D, _) => Binding::Axis1DKey { pos: Key::Space },
    }
}
