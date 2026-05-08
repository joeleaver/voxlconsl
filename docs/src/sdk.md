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

## What's wired up today (v0.0.3)

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

// Misc (§8.3)
fn log(msg: &str);
```

## What's not wired up yet

Most of the spec, honestly — this is pre-alpha. The audio, physics,
actors, pointer input, system actions, and full camera surface
(`Projection`, Euler-style camera setters, view distance, fog, render
rect) are all still TODO. The [roadmap](roadmap.md) tracks priorities.

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
in progress; v0.0.3 carts are raw `.wasm` files.)
