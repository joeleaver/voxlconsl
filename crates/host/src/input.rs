//! Input — see SPEC.md §6.
//!
//! v0.0.3: action declaration + Button polling (held / pressed / released /
//! held_ms) + Axis2D polling. Auto-binds declared actions to a fixed
//! browser-port key-and-mouse set per the §6.6 default tables.
//!
//! TODO:
//!   - Pointer + Axis1D actions
//!   - Reserved system actions (§6.3)
//!   - Per-port binding tables for handheld + touch
//!   - Rebind UI (§6.7)
//!   - Stick deadzone tuning (§6.5)
//!   - Rumble output (§6.8)

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
    pub fn key_event(&mut self, key: Key, down: bool) {
        if down {
            if self.keys_held.insert(key) {
                self.keys_pressed_this_frame.insert(key);
                self.keys_held_ms.insert(key, 0);
            }
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

    /// Called by the runtime once per frame after `cart.update` finishes,
    /// to clear edge-triggered events and roll forward held-time counters.
    pub fn end_of_frame(&mut self, dt_ms: u32) {
        self.keys_pressed_this_frame.clear();
        self.keys_released_this_frame.clear();
        for ms in self.keys_held_ms.values_mut() {
            *ms = ms.saturating_add(dt_ms);
        }
        // Mouse delta is consumed each frame.
        self.mouse_dx = 0.0;
        self.mouse_dy = 0.0;
    }
}

impl Default for InputState {
    fn default() -> Self { Self::new() }
}

/// Default browser-port bindings (subset of SPEC.md §6.6, no-gamepad column).
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
        (ActionKind::Button, PrimaryFire) => Binding::Button { key: Key::J },
        (ActionKind::Button, SecondaryFire) => Binding::Button { key: Key::K },
        (ActionKind::Button, Confirm) => Binding::Button { key: Key::Enter },
        (ActionKind::Button, Cancel) => Binding::Button { key: Key::Escape },
        (ActionKind::Button, Pause) => Binding::Button { key: Key::Tab },
        (ActionKind::Button, Menu) => Binding::Button { key: Key::F1 },
        (ActionKind::Button, _) => Binding::Button { key: Key::Space },
        (ActionKind::Axis1D, _) => Binding::Axis1DKey { pos: Key::Space },
        (ActionKind::Pointer, _) => Binding::None,
    }
}
