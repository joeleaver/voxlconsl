# Hello Cube Walkthrough

This is a guided read of [`examples/hello-cube/src/lib.rs`](https://github.com/joeleaver/voxlconsl/blob/main/examples/hello-cube/src/lib.rs)
— the cart you're seeing render in the [live emulator](emulator.md).

## What it does

- Defines eight materials (stone, wood, leaves, ruby, gold, grass, skin,
  shirt).
- Builds a chequered ground, a voxel tree, a ruby on top, and a few
  scattered gold cubes in `init`.
- Authors **five voxel prefabs** at runtime: an idle dude pose, three
  walk-cycle frames, and a barrel.
- Spawns a **player actor** from the idle prefab and three **barrel
  actors** at three different `Orientation` values to show off the
  24-orientation bake.
- Declares three input actions: `move` (PrimaryMovement), `aim` (Aim),
  and `fire` (PrimaryFire).
- Each frame: drives the player position+yaw from `move`, orbits the
  camera from `aim`, and cycles walk-cycle prefabs via `Flipbook` while
  the dude is moving — snapping back to the idle prefab when stopped.

## Skeleton

```rust
#![no_std]
#![no_main]

use voxlconsl_sdk::*;
use voxlconsl_sdk::animation::Flipbook;

// Prefab IDs — assigned by the cart, opaque to the host.
const P_IDLE:   PrefabId = PrefabId(1);
const P_WALK_0: PrefabId = PrefabId(2);
const P_WALK_1: PrefabId = PrefabId(3);
const P_WALK_2: PrefabId = PrefabId(4);
const P_BARREL: PrefabId = PrefabId(5);

// Cart-global state. `static mut` is the cheapest path during v0.0.x;
// edition-2024 needs `&raw const` / `&raw mut` to take pointers safely.
static mut PLAYER: Option<ActorId> = None;
static mut PLAYER_POS: Vec3 = Vec3 { x: 16.0, y: 1.0, z: 16.0 };

static mut MOVE_ACTION: ActionHandle = ActionHandle(0);
static mut AIM_ACTION:  ActionHandle = ActionHandle(0);
static mut FIRE_ACTION: ActionHandle = ActionHandle(0);

// Walk cycle: WALK_0 → WALK_1 → WALK_2 → WALK_1 → repeat.
const WALK_FRAMES: &[PrefabId] = &[P_WALK_0, P_WALK_1, P_WALK_2, P_WALK_1];
static mut WALK_FB: Flipbook = Flipbook::new(WALK_FRAMES, 140, true);
```

## `init`: world, prefabs, actors, actions

`init()` runs once at cart boot. It populates the material table,
authors prefab volumes, spawns actors, and declares input actions.

### Materials, sky, sun, world geometry

```rust
material_define(M_STONE, Material::pack_color(14, 1), 0, MaterialFlags::empty());
material_define(M_GRASS, Material::pack_color( 3, 2), 0, MaterialFlags::empty());
// ...

sky_set_gradient(Material::pack_color(7, 0), Material::pack_color(6, 0));
light_set_sun(Vec3::new(-0.6, 0.8, 0.4), 0, 0);

for x in 0..32 {
    for z in 0..32 {
        let m = if (x + z) % 2 == 0 { M_STONE } else { M_GRASS };
        set_voxel(UVec3::new(x, 0, z), m);
    }
}
// Tree, ruby, gold cubes... (see source.)
```

### Prefab definition

The cart fills a `static mut [u8; W*H*D]` for each frame, then registers
each one with the host via `prefab_define`. The host copies the bytes
into its prefab table — the cart can drop its buffers right after.

```rust
unsafe {
    build_dude(&mut *(&raw mut DENSE_IDLE),   /*l*/ 1, /*r*/ 1, /*al*/ 1, /*ar*/ 1);
    build_dude(&mut *(&raw mut DENSE_WALK_0), /*l*/ 0, /*r*/ 2, /*al*/ 2, /*ar*/ 0);
    build_dude(&mut *(&raw mut DENSE_WALK_1), /*l*/ 1, /*r*/ 1, /*al*/ 1, /*ar*/ 1);
    build_dude(&mut *(&raw mut DENSE_WALK_2), /*l*/ 2, /*r*/ 0, /*al*/ 0, /*ar*/ 2);

    let dude_size = U8Vec3::new(5, 7, 3);
    prefab_define(P_IDLE,   &*(&raw const DENSE_IDLE),   dude_size);
    prefab_define(P_WALK_0, &*(&raw const DENSE_WALK_0), dude_size);
    prefab_define(P_WALK_1, &*(&raw const DENSE_WALK_1), dude_size);
    prefab_define(P_WALK_2, &*(&raw const DENSE_WALK_2), dude_size);
}
```

Each walk frame swings the foot/arm offsets in the volume's z-axis, so
prefab-swap reads as a walking cycle.

> The `&raw const` / `&raw mut` syntax is edition-2024's replacement
> for `&STATIC_MUT` references; it materializes a raw pointer without
> creating a Rust reference, which keeps `static_mut_refs` happy.
> `prefab_define` is a v0.0.x stand-in for the `.voxl` cart format
> path that will eventually load prefabs at boot — see SPEC.md §11.4.

### Spawning the player + barrels

```rust
let id = actor_spawn_from(P_IDLE, Orientation::Up).expect("player");
unsafe {
    PLAYER = Some(id);
    actor_set_position(id, PLAYER_POS);
}

// Three barrels showing off the orientation bake (§11.3).
if let Some(b) = actor_spawn_from(P_BARREL, Orientation::Up)      { actor_set_position(b, Vec3::new(2.0, 1.0, 4.0)); }
if let Some(b) = actor_spawn_from(P_BARREL, Orientation::EastUp)  { actor_set_position(b, Vec3::new(2.0, 1.0, 12.0)); }
if let Some(b) = actor_spawn_from(P_BARREL, Orientation::NorthUp) { actor_set_position(b, Vec3::new(2.0, 1.0, 20.0)); }
```

`actor_spawn_from` looks up `(prefab, orientation)` in the host's bake
cache and gives the actor a shared `Rc` reference to the baked volume.
Two actors instancing the same prefab+orientation share one buffer
(copy-on-write); a non-`Up` orientation triggers a one-time rotation
bake the next instance reuses for free. See SPEC.md §11.4 / §11.5 for
the full bake-trigger table.

### Action declaration

```rust
unsafe {
    MOVE_ACTION = input_declare_action(ActionKind::Axis2D, BindingHint::PrimaryMovement, "move");
    AIM_ACTION  = input_declare_action(ActionKind::Axis2D, BindingHint::Aim, "aim");
    FIRE_ACTION = input_declare_action(ActionKind::Button, BindingHint::PrimaryFire, "fire");
}
```

The cart never picks "WASD" or "mouse" itself. It says *"I want a 2D
primary-movement action"* and the platform decides what that maps to on
the current port.

## `update`: read input, drive the player + camera + animation

```rust
pub extern "C" fn update(dt_ms: u32) {
    let dt = (dt_ms as f32) / 1000.0;
    let (mx, my) = input_action_axis2d(unsafe { MOVE_ACTION });
    let (ax, ay) = input_action_axis2d(unsafe { AIM_ACTION });

    unsafe {
        CAM_YAW += ax * 0.005;
        CAM_PITCH = (CAM_PITCH - ay * 0.005).clamp(-1.2, 1.2);
    }

    let cam_yaw = unsafe { CAM_YAW };
    let forward = Vec3::new(sine(cam_yaw), 0.0, cosine(cam_yaw));
    let right   = Vec3::new(cosine(cam_yaw), 0.0, -sine(cam_yaw));
    let movement = Vec3::new(
        right.x * mx + forward.x * my, 0.0, right.z * mx + forward.z * my,
    );
    let moving = movement.x.abs() + movement.z.abs() > 0.05;

    if let Some(player) = unsafe { PLAYER } {
        unsafe {
            PLAYER_POS.x = (PLAYER_POS.x + movement.x * 6.0 * dt).clamp(0.0, 27.0);
            PLAYER_POS.z = (PLAYER_POS.z + movement.z * 6.0 * dt).clamp(0.0, 29.0);
            actor_set_position(player, PLAYER_POS);
            if moving {
                actor_set_yaw(player, -atan2(movement.x, movement.z));
            }

            // Animation: cycle walk frames while moving, snap back to
            // idle when stopped. Only call set_prefab on transitions.
            let walk_fb = &mut *(&raw mut WALK_FB);
            let want = if moving { walk_fb.tick(dt_ms); walk_fb.current() }
                       else      { walk_fb.reset();    P_IDLE };
            if want != CURRENT_FRAME {
                actor_set_prefab(player, want);
                CURRENT_FRAME = want;
            }
        }
    }
}
```

Three things going on:

1. **`input_action_axis2d`** returns the action's current value. For
   `move` (PrimaryMovement) on the browser port, that's WASD packed as
   `(±1, ±1)`. For `aim` (Aim), it's the mouse delta this frame.
2. **`actor_set_prefab`** is a pointer-cheap swap on the host — the
   actor's volume reference is rotated through the bake cache. With
   the CoW prefab system, twenty walking dudes would share four baked
   volumes; this is the basis of flipbook animation (§11.9).
3. **The cart owns its camera state.** There is no persistent "camera"
   object on the host — the cart calls `camera_set_lookat` each frame
   in `render()` with whatever `eye` / `target` it computed.

## `render`: configure the camera

```rust
pub extern "C" fn render() {
    let (yaw, pitch, dist) = unsafe { (CAM_YAW, CAM_PITCH, CAM_DISTANCE) };
    let pos = unsafe { PLAYER_POS };

    let cos_pitch = cosine(pitch);
    let target = Vec3::new(pos.x + 2.5, pos.y + 4.0, pos.z + 1.5);
    let eye = Vec3::new(
        target.x + dist * sine(yaw) * cos_pitch,
        target.y + dist * sine(pitch),
        target.z + dist * cosine(yaw) * cos_pitch,
    );

    camera_set_lookat(eye, target, Vec3::Y);
    camera_set_fov(60.0);
}
```

`render` is where the cart commits camera/lighting state for this
frame. The host then ray-marches with whatever's been set, compositing
the world chunk and every visible actor (player + barrels) into one
depth comparison.

## What's intentionally hand-rolled

- `sine` / `cosine` / `atan2` — tiny no_std polynomials. The cart could
  pull in `libm` instead, but for ~20 lines it's not worth the
  dependency.
- `static mut` for cart state — the v0.0.x story. We'll move to a more
  structured approach (a thin `State` struct passed via `OnceCell`,
  or similar) when more carts exist.
- `build_dude` builds prefab dense buffers procedurally rather than
  loading them from a `.vxv` file; that path lands when the bundler
  and `.voxl` cart format do.

## Build it yourself

```sh
git clone https://github.com/joeleaver/voxlconsl
cd voxlconsl
cargo build --target wasm32-unknown-unknown --release -p hello-cube
ls -lh target/wasm32-unknown-unknown/release/hello_cube.wasm
```
