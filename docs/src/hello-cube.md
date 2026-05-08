# Hello Cube Walkthrough

This is a guided read of [`examples/hello-cube/src/lib.rs`](https://github.com/joeleaver/voxlconsl/blob/main/examples/hello-cube/src/lib.rs)
— the 3.5 KB cart you're seeing render in the [live emulator](emulator.md).

## What it does

- Defines six materials (stone, wood, leaves, ruby, gold, grass).
- Builds a chequered ground, a voxel tree with leaf canopy, a ruby on
  top, and five gold cubes in `init`.
- Declares three input actions: a 2D movement (orbit camera), a 2D aim
  (mouse look), and a button (cycle the ruby's shade).
- Drives a spherical-coordinate orbit camera in `update`/`render`.

## Skeleton

```rust
#![no_std]
#![no_main]

use voxlconsl_sdk::*;

// Cart-global state. `static mut` is the cheapest way during v0.0.x.
static mut CAM_YAW: f32 = 0.0;
static mut CAM_PITCH: f32 = 0.4;
static mut CAM_DISTANCE: f32 = 38.0;

static mut MOVE_ACTION: ActionHandle = ActionHandle(0);
static mut AIM_ACTION: ActionHandle = ActionHandle(0);
static mut FIRE_ACTION: ActionHandle = ActionHandle(0);
```

## `init`: world + actions

`init()` runs once at cart boot. It populates the material table, builds
geometry, and declares input actions:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn init() {
    // Materials use packed (ramp << 2) | shade color bytes.
    material_define(M_STONE, Material::pack_color(14, 1), 0, MaterialFlags::empty());
    material_define(M_WOOD,  Material::pack_color( 0, 1), 0, MaterialFlags::empty());
    // ... etc

    // Sky + sun.
    sky_set_gradient(Material::pack_color(7, 0), Material::pack_color(6, 0));
    light_set_sun(Vec3::new(-0.6, 0.8, 0.4), 0, 0);

    // Chequered ground via individual set_voxel calls.
    for x in 0..32u32 {
        for z in 0..32u32 {
            let m = if (x + z) % 2 == 0 { M_STONE } else { M_GRASS };
            set_voxel(UVec3::new(x, 0, z), m);
        }
    }

    // Tree, leaves, ruby, gold cubes... (see source for the rest)

    // Declare actions and store the handles globally.
    unsafe {
        MOVE_ACTION = input_declare_action(ActionKind::Axis2D, BindingHint::PrimaryMovement, "move");
        AIM_ACTION  = input_declare_action(ActionKind::Axis2D, BindingHint::Aim, "aim");
        FIRE_ACTION = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire, "fire");
    }
}
```

Note: the cart never picks "WASD" or "mouse" itself. It says
*"I want a 2D primary-movement action"* and the platform decides what
that maps to on the current port.

## `update`: read input, advance state

```rust
#[unsafe(no_mangle)]
pub extern "C" fn update(dt_ms: u32) {
    let dt = (dt_ms as f32) / 1000.0;
    let (mx, my) = input_action_axis2d(unsafe { MOVE_ACTION });
    let (ax, ay) = input_action_axis2d(unsafe { AIM_ACTION });

    unsafe {
        CAM_YAW += mx * 2.0 * dt + ax * 0.005;
        CAM_PITCH += -ay * 0.005;
        CAM_PITCH = CAM_PITCH.clamp(-1.2, 1.2);
        CAM_DISTANCE -= my * 18.0 * dt;
        CAM_DISTANCE = CAM_DISTANCE.clamp(8.0, 80.0);

        // Edge-detected button press cycles the ruby's shade.
        if input_action_pressed(FIRE_ACTION) {
            // mutate M_RUBY's material at runtime
        }
    }
}
```

Three things going on here:

1. **`input_action_axis2d`** returns the action's current value. For
   `move` (PrimaryMovement) on the browser port, that's WASD packed as
   `(±1, ±1)`. For `aim` (Aim) it's the mouse delta this frame.
2. **`input_action_pressed`** is edge-triggered. It returns true exactly
   on the frame the player presses `J` (the browser port's default for
   PrimaryFire), regardless of how long they hold it.
3. The cart owns its camera state. The platform doesn't have a
   "camera" object you mutate — the cart is responsible for `eye`,
   `target`, and any easing it wants to apply.

## `render`: configure the camera

```rust
#[unsafe(no_mangle)]
pub extern "C" fn render() {
    let (yaw, pitch, dist) = unsafe { (CAM_YAW, CAM_PITCH, CAM_DISTANCE) };
    let target = Vec3::new(16.0, 6.0, 16.0);

    let cos_pitch = cosine(pitch);
    let eye = Vec3::new(
        target.x + dist * sine(yaw) * cos_pitch,
        target.y + dist * sine(pitch),
        target.z + dist * cosine(yaw) * cos_pitch,
    );

    camera_set_lookat(eye, target, Vec3::Y);
    camera_set_fov(60.0);
}
```

`render` is where the cart commits camera/lighting state for this frame.
The host then ray-marches with whatever's been set.

## What's intentionally hand-rolled

- `sine`/`cosine` — a tiny no_std polynomial. The cart could pull in
  `libm` instead, but for ~5 lines it's not worth the dependency.
- `static mut` for cart state — the v0.0.x story. We'll move to a more
  structured approach (a thin `State` struct passed via `OnceCell`,
  or similar) when more carts exist.

## Build it yourself

```sh
git clone https://github.com/joeleaver/voxlconsl
cd voxlconsl
cargo build --target wasm32-unknown-unknown --release -p hello-cube
ls -lh target/wasm32-unknown-unknown/release/hello_cube.wasm
```

You'll see something close to 3.5 KB, depending on toolchain version.
