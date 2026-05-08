# SDK Reference

The cart-side crate `voxlconsl-sdk` is what cart authors depend on. It
re-exports the shared types from `voxlconsl-types` and wraps every host
import as a safe Rust function.

This page is a quick orientation. The
[Hello Cube walkthrough](hello-cube.md) shows the SDK in real cart code,
and [§8 of the spec](spec.md#8-host-api-surface-cart-host) is the
authoritative index of every function.

## Cart entry points

A cart exports exactly three functions:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn init() { ... }      // once at boot

#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) { ... }   // each frame

#[unsafe(no_mangle)]
pub extern "C" fn render() { ... }    // each frame, after update
```

Carts must export all three. The host treats missing exports as a load
error.

## What's wired up today (v0.0.6)

```rust
// World mutation (§3.6)
fn set_voxel(pos: UVec3, material: u8);
fn fill_box(min: UVec3, max: UVec3, material: u8);
fn clear_world();

// Materials (§2)
fn material_define(slot: u8, color: u8, emission: u8, flags: MaterialFlags);

// Camera (§3.2)
fn camera_set_lookat(eye: Vec3, target: Vec3, up: Vec3);
fn camera_set_fov(fov_y_deg: f32);

// Lighting + sky (§3.3, §3.4)
fn light_set_sun(direction: Vec3, color: u8, intensity: u8);
fn sky_set_gradient(top: u8, horizon: u8);

// Input (§6)
fn input_declare_action(kind: ActionKind, hint: BindingHint, name: &str) -> ActionHandle;
fn input_action_button(h: ActionHandle) -> bool;
fn input_action_pressed(h: ActionHandle) -> bool;
fn input_action_released(h: ActionHandle) -> bool;
fn input_action_held_ms(h: ActionHandle) -> u32;
fn input_action_axis1d(h: ActionHandle) -> f32;
fn input_action_axis2d(h: ActionHandle) -> (f32, f32);
fn input_action_active(h: ActionHandle) -> bool;

// Actors (§11) — lifecycle
fn actor_spawn() -> Option<ActorId>;
fn actor_spawn_from(prefab: PrefabId, orientation: Orientation) -> Option<ActorId>;
fn actor_despawn(actor: ActorId);
fn actor_count() -> u32;

// Actors — transform
fn actor_set_position(actor: ActorId, pos: Vec3);
fn actor_get_position(actor: ActorId) -> Vec3;
fn actor_set_yaw(actor: ActorId, yaw: f32);
fn actor_get_yaw(actor: ActorId) -> f32;
fn actor_set_orientation(actor: ActorId, orientation: Orientation);   // §11.5
fn actor_get_orientation(actor: ActorId) -> Orientation;
fn actor_set_visible(actor: ActorId, visible: bool);

// Actors — volume editing (forks prefab-shared actors on first edit)
fn actor_set_voxel(actor: ActorId, pos: U8Vec3, material: u8);
fn actor_fill_box(actor: ActorId, min: U8Vec3, max: U8Vec3, material: u8);
fn actor_clear(actor: ActorId);

// Actors — prefab swap (basis of flipbook animation, §11.9)
fn actor_set_prefab(actor: ActorId, prefab: PrefabId);

// Prefabs (§11.4) — v0.0.x stand-in for §7 cart-format-driven loading
fn prefab_define(prefab: PrefabId, dense: &[u8], size: U8Vec3);

// Misc (§8.3)
fn log(msg: &str);
```

### Animation

The SDK ships a small cart-side animation helper at
`voxlconsl_sdk::animation::Flipbook`. It cycles an actor through a list
of prefab IDs over time — the v1 animation model per
[§11.9 of the spec](spec.md#119-animation).

```rust
use voxlconsl_sdk::animation::Flipbook;

const WALK_FRAMES: &[PrefabId] = &[WALK_0, WALK_1, WALK_2, WALK_1];
static mut WALK: Flipbook = Flipbook::new(WALK_FRAMES, 120, true);

fn update(dt_ms: u32) {
    let clip = unsafe { &mut *(&raw mut WALK) };
    clip.tick(dt_ms);
    actor_set_prefab(player_actor, clip.current());

    if clip.just_entered_frame(0) { /* play left footstep SFX */ }
    if clip.just_entered_frame(2) { /* play right footstep SFX */ }
}
```

Pure cart-side. The host doesn't track animation state — it just
receives prefab swaps. The CoW prefab system (§11.4) makes prefab-swap
effectively free: every prefab is baked once per
`(prefab, orientation)` pair across the whole cart, and any actor
playing the animation just rotates a pointer reference through the
baked-volume cache. Twenty walking dudes share four baked volumes.

See the [spec §11.9 rationale](spec.md#119-animation) for why flipbook
(and not skeletal) is the right fit for voxels.

### Orientations

The 24 cube-symmetry orientations (§11.3) are exposed on
`voxlconsl_types::Orientation` and re-exported by the SDK. Each variant
is a `(up_world, fwd_world)` pair of signed unit axes:

```rust
// Pre-bake a barrel at three orientations: upright, tipped east, tipped north.
let a = actor_spawn_from(P_BARREL, Orientation::Up).unwrap();
let b = actor_spawn_from(P_BARREL, Orientation::EastUp).unwrap();
let c = actor_spawn_from(P_BARREL, Orientation::NorthUp).unwrap();

// Re-orient at runtime — pointer-cheap if the actor is still
// prefab-shared; rotates the dense in place for owned (post-edit) actors.
actor_set_orientation(a, Orientation::UpRot90);
```

The host bakes one volume per unique `(prefab, orientation)` pair and
shares duplicates via copy-on-write. See SPEC.md §11.4 / §11.5 for the
full bake-trigger table.

## What's not wired up yet

Plenty of the spec — this is pre-alpha. The audio, physics queries,
pointer input, system actions, full camera surface (`Projection`,
`camera_set_euler`, view distance, fog, render rect), CA simulation,
and multi-chunk world are all still TODO. The [roadmap](roadmap.md)
tracks priorities.

## Setting up a cart project

```toml
# Cargo.toml
[package]
name = "my-cart"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
voxlconsl-sdk = { git = "https://github.com/joeleaver/voxlconsl" }
```

```rust
// src/lib.rs
#![no_std]
#![no_main]

use voxlconsl_sdk::*;

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    material_define(1, Material::pack_color(/* sky_blue */ 6, 1), 0, MaterialFlags::empty());
    fill_box(UVec3::new(0, 0, 0), UVec3::new(31, 0, 31), 1);
    sky_set_gradient(Material::pack_color(7, 0), Material::pack_color(6, 0));
    light_set_sun(Vec3::new(-0.6, 0.8, 0.4), 0, 0);
}

#[unsafe(no_mangle)]
pub extern "C" fn update(_dt_ms: u32) {}

#[unsafe(no_mangle)]
pub extern "C" fn render() {
    camera_set_lookat(
        Vec3::new(50.0, 30.0, 50.0),
        Vec3::new(16.0, 1.0, 16.0),
        Vec3::Y,
    );
    camera_set_fov(60.0);
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { loop {} }
```

Build with:

```sh
cargo build --target wasm32-unknown-unknown --release
```

The output `.wasm` is a complete cart binary. (Cart format support — the
real `.voxl` container with metadata, materials, audio, etc. — is still
in progress; v0.0.x carts are raw `.wasm` files.)
