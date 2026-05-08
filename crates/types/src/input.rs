//! Action-based input model — see SPEC.md §6.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ActionKind {
    /// Discrete: held / pressed / released / held_ms.
    Button,
    /// `f32` in -1..1 (signed) or 0..1 (unsigned by binding).
    Axis1D,
    /// `(f32, f32)` inside the unit disc — sticks or aim deltas.
    Axis2D,
    /// `(i16, i16)` absolute, framebuffer pixel coords.
    Pointer,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BindingHint {
    /// Platform infers from `kind` alone.
    None,
    /// 2D ground/world movement (Axis2D).
    PrimaryMovement,
    /// 2D look/aim — accepts stick or pointer-delta (Axis2D).
    Aim,
    /// Main "do it" button (Button).
    PrimaryFire,
    /// Alt fire / aim-down-sights / right click.
    SecondaryFire,
    /// Dialog / UI semantics (Button).
    Confirm,
    Cancel,
    /// System-flavored (Button).
    Menu,
    Pause,
    /// Requires pointer; cart should degrade if absent.
    PointerOnly,
}

/// Cart's declaration of one action.
///
/// `name` is used by the system rebind UI; never crosses the host boundary
/// at frame time after declaration.
#[derive(Copy, Clone, Debug)]
pub struct ActionDecl<'a> {
    pub name: &'a str,
    pub kind: ActionKind,
    pub hint: BindingHint,
}

/// Opaque handle the cart receives at declaration time.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ActionHandle(pub u32);

impl ActionHandle {
    /// Reserved system action: pause control. Always present.
    pub const SYSTEM_PAUSE: Self = Self(0xFFFF_FFFE);
    /// Reserved system action: platform menu. Always present.
    pub const SYSTEM_MENU: Self = Self(0xFFFF_FFFF);
}
