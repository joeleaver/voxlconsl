//! Cart sandbox — runs cart `.wasm` modules under `wasmi`.
//!
//! See SPEC.md §9 (browser host) and §8 (host API surface).
//!
//! v0.0.3 surface (cart imports):
//!
//!   set_voxel(x, y, z, material)
//!   fill_box(min_x, min_y, min_z, max_x, max_y, max_z, material)
//!   clear_world()
//!   material_define(slot, color, emission, flags)
//!   camera_set_lookat(ex, ey, ez, tx, ty, tz, ux, uy, uz)
//!   camera_set_fov(fov_y_deg)
//!   light_set_sun(dx, dy, dz, _color, _intensity)
//!   sky_set_gradient(top, horizon)
//!
//! v0.0.3 cart exports: `init`, `update(dt_ms)`, `render()`.

use wasmi::{Caller, Engine, Linker, Module, Store, TypedFunc};

use voxlconsl_types::{
    ActionKind, ActorId, BindingHint, Material, MaterialFlags, Orientation, PrefabId,
    U8Vec3, Vec3,
    cart_format::{Cart as VoxlCart, CartError as VoxlError, MAGIC as VOXL_MAGIC},
};

use crate::renderer::Camera;
use crate::world::WorldState;

/// A loaded, ready-to-tick cart.
///
/// `init` has already been called by `Cart::load`; what remains is per-frame
/// `update` and `render`.
pub struct Cart {
    store: Store<WorldState>,
    update_fn: TypedFunc<u32, ()>,
    render_fn: TypedFunc<(), ()>,
}

#[derive(Debug)]
pub enum CartError {
    Wasm(wasmi::Error),
    MissingExport(&'static str),
    Voxl(VoxlError),
}

impl From<wasmi::Error> for CartError {
    fn from(e: wasmi::Error) -> Self { Self::Wasm(e) }
}

impl From<VoxlError> for CartError {
    fn from(e: VoxlError) -> Self { Self::Voxl(e) }
}

impl Cart {
    /// Load and instantiate a cart. Accepts either a `.voxl` cart binary
    /// (auto-detected via the `VOXLCONSL\0` magic) or raw `.wasm` bytes
    /// for tests and migration. Calls the cart's `init` export
    /// immediately so the cart can populate the world.
    pub fn load(bytes: &[u8]) -> Result<Self, CartError> {
        // Auto-detect: try the .voxl path when the magic matches; fall
        // back to raw WASM otherwise. Lets us flip browser-host /
        // CLI / tests at our own pace.
        let wasm_bytes: &[u8] = if bytes.len() >= VOXL_MAGIC.len() && bytes[..VOXL_MAGIC.len()] == VOXL_MAGIC {
            let cart = VoxlCart::parse(bytes)?;
            cart.code()
        } else {
            bytes
        };

        let engine = Engine::default();
        let module = Module::new(&engine, wasm_bytes)?;
        let mut store = Store::new(&engine, WorldState::new());

        let mut linker = Linker::new(&engine);
        register_host_imports(&mut linker)?;

        let pre = linker.instantiate(&mut store, &module)?;
        let instance = pre.start(&mut store)?;

        let init_fn = instance
            .get_typed_func::<(), ()>(&store, "init")
            .map_err(|_| CartError::MissingExport("init"))?;
        let update_fn = instance
            .get_typed_func::<u32, ()>(&store, "update")
            .map_err(|_| CartError::MissingExport("update"))?;
        let render_fn = instance
            .get_typed_func::<(), ()>(&store, "render")
            .map_err(|_| CartError::MissingExport("render"))?;

        // Run cart init() so it can populate the world.
        init_fn.call(&mut store, ())?;

        Ok(Self { store, update_fn, render_fn })
    }

    pub fn update(&mut self, dt_ms: u32) -> Result<(), CartError> {
        self.update_fn.call(&mut self.store, dt_ms)?;
        Ok(())
    }

    pub fn render(&mut self) -> Result<(), CartError> {
        self.render_fn.call(&mut self.store, ())?;
        Ok(())
    }

    pub fn world(&mut self) -> &mut WorldState {
        self.store.data_mut()
    }
}

fn register_host_imports(linker: &mut Linker<WorldState>) -> Result<(), wasmi::Error> {
    // World mutation (§3.6)
    linker.func_wrap(
        "env", "set_voxel",
        |mut caller: Caller<WorldState>, x: u32, y: u32, z: u32, material: u32| {
            caller.data_mut().set_voxel(x, y, z, material as u8);
        },
    )?;
    linker.func_wrap(
        "env", "fill_box",
        |mut caller: Caller<WorldState>,
         min_x: u32, min_y: u32, min_z: u32,
         max_x: u32, max_y: u32, max_z: u32,
         material: u32| {
            caller.data_mut().fill_box(min_x, min_y, min_z, max_x, max_y, max_z, material as u8);
        },
    )?;
    linker.func_wrap(
        "env", "clear_world",
        |mut caller: Caller<WorldState>| {
            caller.data_mut().clear_world();
        },
    )?;

    // Scenes (§3.7) — multi-scene mutable voxel grids. Each scene is a
    // 512³ world; the cart can address up to 256 of them. Mutations
    // and the renderer always target the active scene.
    linker.func_wrap(
        "env", "scene_set_active",
        |mut caller: Caller<WorldState>, id: u32| {
            // u32 → u8 is a low-byte truncation; values ≥ 256 map onto
            // a different scene rather than being rejected. Cart-side
            // SDK enforces the SceneId(u8) at the type level.
            caller.data_mut().scene_set_active(id as u8);
        },
    )?;
    linker.func_wrap(
        "env", "scene_get_active",
        |caller: Caller<WorldState>| -> u32 {
            caller.data().scene_get_active() as u32
        },
    )?;

    // Material table (v0.0.3 placeholder for materials.toml-driven loading)
    linker.func_wrap(
        "env", "material_define",
        |mut caller: Caller<WorldState>,
         slot: u32, color: u32, emission: u32, flags: u32| {
            let mat = Material {
                color: color as u8,
                emission: emission as u8,
                flags: MaterialFlags(flags as u16),
                ca_threshold: 0,
                ca_lifetime: 0,
                ca_viscosity: 0,
                _reserved: [0; 9],
            };
            caller.data_mut().set_material(slot as u8, mat);
        },
    )?;

    // Camera (§3.2)
    linker.func_wrap(
        "env", "camera_set_lookat",
        |mut caller: Caller<WorldState>,
         ex: f32, ey: f32, ez: f32,
         tx: f32, ty: f32, tz: f32,
         ux: f32, uy: f32, uz: f32| {
            let world = caller.data_mut();
            world.camera = Camera {
                eye: Vec3::new(ex, ey, ez),
                target: Vec3::new(tx, ty, tz),
                up: Vec3::new(ux, uy, uz),
                fov_y_deg: world.camera.fov_y_deg,
            };
        },
    )?;
    linker.func_wrap(
        "env", "camera_set_fov",
        |mut caller: Caller<WorldState>, fov_y_deg: f32| {
            caller.data_mut().camera.fov_y_deg = fov_y_deg;
        },
    )?;

    // Lighting (§3.3) — color/intensity unused in v0.0.3's flat model
    linker.func_wrap(
        "env", "light_set_sun",
        |mut caller: Caller<WorldState>,
         dx: f32, dy: f32, dz: f32,
         _color: u32, _intensity: u32| {
            caller.data_mut().sun_dir = Vec3::new(dx, dy, dz);
        },
    )?;

    // Sky (§3.4)
    linker.func_wrap(
        "env", "sky_set_gradient",
        |mut caller: Caller<WorldState>, top: u32, horizon: u32| {
            let world = caller.data_mut();
            world.sky_top = top as u8;
            world.sky_horizon = horizon as u8;
        },
    )?;

    // Actors (§11.7) — minimum-viable v0.0.4 surface. `actor_set_prefab`
    // remains a no-op stub since prefabs/CoW arrive later. Spawn / despawn /
    // transform / volume editing are real.
    linker.func_wrap(
        "env", "actor_spawn",
        |mut caller: Caller<WorldState>| -> u32 {
            caller
                .data_mut()
                .actors
                .spawn()
                .map(|id| id.0)
                .unwrap_or(u32::MAX)
        },
    )?;
    linker.func_wrap(
        "env", "actor_despawn",
        |mut caller: Caller<WorldState>, id: u32| {
            caller.data_mut().actors.despawn(ActorId(id));
        },
    )?;
    linker.func_wrap(
        "env", "actor_count",
        |caller: Caller<WorldState>| -> u32 {
            caller.data().actors.count()
        },
    )?;
    linker.func_wrap(
        "env", "actor_set_position",
        |mut caller: Caller<WorldState>, id: u32, x: f32, y: f32, z: f32| {
            if let Some(a) = caller.data_mut().actors.get_mut(ActorId(id)) {
                a.position = Vec3::new(x, y, z);
            }
        },
    )?;
    linker.func_wrap(
        "env", "actor_get_position",
        |mut caller: Caller<WorldState>, id: u32, out_x: u32, out_y: u32, out_z: u32| {
            let p = caller
                .data()
                .actors
                .get(ActorId(id))
                .map(|a| a.position)
                .unwrap_or(Vec3::ZERO);
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return,
            };
            let _ = memory.write(&mut caller, out_x as usize, &p.x.to_le_bytes());
            let _ = memory.write(&mut caller, out_y as usize, &p.y.to_le_bytes());
            let _ = memory.write(&mut caller, out_z as usize, &p.z.to_le_bytes());
        },
    )?;
    linker.func_wrap(
        "env", "actor_set_yaw",
        |mut caller: Caller<WorldState>, id: u32, yaw: f32| {
            if let Some(a) = caller.data_mut().actors.get_mut(ActorId(id)) {
                a.yaw = yaw;
            }
        },
    )?;
    linker.func_wrap(
        "env", "actor_get_yaw",
        |caller: Caller<WorldState>, id: u32| -> f32 {
            caller.data().actors.get(ActorId(id)).map(|a| a.yaw).unwrap_or(0.0)
        },
    )?;
    linker.func_wrap(
        "env", "actor_set_visible",
        |mut caller: Caller<WorldState>, id: u32, visible: u32| {
            if let Some(a) = caller.data_mut().actors.get_mut(ActorId(id)) {
                a.visible = visible != 0;
            }
        },
    )?;
    linker.func_wrap(
        "env", "actor_set_voxel",
        |mut caller: Caller<WorldState>, id: u32, x: u32, y: u32, z: u32, material: u32| {
            if let Some(a) = caller.data_mut().actors.get_mut(ActorId(id)) {
                a.set_voxel(x as u8, y as u8, z as u8, material as u8);
            }
        },
    )?;
    linker.func_wrap(
        "env", "actor_fill_box",
        |mut caller: Caller<WorldState>, id: u32,
         min_x: u32, min_y: u32, min_z: u32,
         max_x: u32, max_y: u32, max_z: u32,
         material: u32| {
            if let Some(a) = caller.data_mut().actors.get_mut(ActorId(id)) {
                a.fill_box(
                    U8Vec3::new(min_x as u8, min_y as u8, min_z as u8),
                    U8Vec3::new(max_x as u8, max_y as u8, max_z as u8),
                    material as u8,
                );
            }
        },
    )?;
    linker.func_wrap(
        "env", "actor_clear",
        |mut caller: Caller<WorldState>, id: u32| {
            if let Some(a) = caller.data_mut().actors.get_mut(ActorId(id)) {
                a.clear();
            }
        },
    )?;

    // Prefabs (§11.4)
    //
    // `prefab_define` is the v0.0.5 stand-in for the cart-format-driven
    // path: the cart copies a dense voxel buffer into the host's prefab
    // table at init time. Once the §7 cart format lands, the runtime
    // populates the same table from the World section before `init` runs
    // and this import becomes optional.
    linker.func_wrap(
        "env", "prefab_define",
        |mut caller: Caller<WorldState>,
         prefab_id: u32, ptr: u32, len: u32,
         sx: u32, sy: u32, sz: u32| {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return,
            };
            let mut buf = vec![0u8; len as usize];
            if memory.read(&caller, ptr as usize, &mut buf).is_err() {
                return;
            }
            let size = U8Vec3::new(sx as u8, sy as u8, sz as u8);
            caller
                .data_mut()
                .prefabs
                .define(PrefabId(prefab_id as u16), buf, size);
        },
    )?;
    linker.func_wrap(
        "env", "actor_spawn_from",
        |mut caller: Caller<WorldState>, prefab_id: u32, orientation: u32| -> u32 {
            let ori = orientation_from_u32(orientation);
            let world = caller.data_mut();
            world
                .actors
                .spawn_from(PrefabId(prefab_id as u16), ori, &mut world.prefabs)
                .map(|id| id.0)
                .unwrap_or(u32::MAX)
        },
    )?;
    linker.func_wrap(
        "env", "actor_set_prefab",
        |mut caller: Caller<WorldState>, actor_id: u32, prefab_id: u32| {
            let world = caller.data_mut();
            world.actors.set_actor_prefab(
                ActorId(actor_id),
                PrefabId(prefab_id as u16),
                &mut world.prefabs,
            );
        },
    )?;
    linker.func_wrap(
        "env", "actor_set_orientation",
        |mut caller: Caller<WorldState>, actor_id: u32, orientation: u32| {
            let ori = orientation_from_u32(orientation);
            let world = caller.data_mut();
            world.actors.set_actor_orientation(ActorId(actor_id), ori, &mut world.prefabs);
        },
    )?;
    linker.func_wrap(
        "env", "actor_get_orientation",
        |caller: Caller<WorldState>, actor_id: u32| -> u32 {
            caller
                .data()
                .actors
                .get(ActorId(actor_id))
                .map(|a| a.orientation as u32)
                .unwrap_or(0)
        },
    )?;

    // Input (§6) — declaration is one-action-at-a-time; cart calls
    // input_declare_action N times during init() and stores the handles.
    linker.func_wrap(
        "env", "input_declare_action",
        |mut caller: Caller<WorldState>,
         kind: u32, hint: u32, name_ptr: u32, name_len: u32| -> u32 {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return u32::MAX,
            };
            let mut buf = vec![0u8; name_len as usize];
            if memory.read(&caller, name_ptr as usize, &mut buf).is_err() {
                return u32::MAX;
            }
            let name = String::from_utf8_lossy(&buf).into_owned();
            let kind = match kind {
                0 => ActionKind::Button,
                1 => ActionKind::Axis1D,
                2 => ActionKind::Axis2D,
                3 => ActionKind::Pointer,
                _ => return u32::MAX,
            };
            let hint = match hint {
                0 => BindingHint::None,
                1 => BindingHint::PrimaryMovement,
                2 => BindingHint::Aim,
                3 => BindingHint::PrimaryFire,
                4 => BindingHint::SecondaryFire,
                5 => BindingHint::Confirm,
                6 => BindingHint::Cancel,
                7 => BindingHint::Menu,
                8 => BindingHint::Pause,
                9 => BindingHint::PointerOnly,
                _ => BindingHint::None,
            };
            caller.data_mut().input.declare(name, kind, hint).0
        },
    )?;
    linker.func_wrap(
        "env", "input_action_button",
        |caller: Caller<WorldState>, h: u32| -> u32 {
            caller.data().input.button(voxlconsl_types::ActionHandle(h)) as u32
        },
    )?;
    linker.func_wrap(
        "env", "input_action_pressed",
        |caller: Caller<WorldState>, h: u32| -> u32 {
            caller.data().input.button_pressed(voxlconsl_types::ActionHandle(h)) as u32
        },
    )?;
    linker.func_wrap(
        "env", "input_action_released",
        |caller: Caller<WorldState>, h: u32| -> u32 {
            caller.data().input.button_released(voxlconsl_types::ActionHandle(h)) as u32
        },
    )?;
    linker.func_wrap(
        "env", "input_action_held_ms",
        |caller: Caller<WorldState>, h: u32| -> u32 {
            caller.data().input.button_held_ms(voxlconsl_types::ActionHandle(h))
        },
    )?;
    linker.func_wrap(
        "env", "input_action_axis1d",
        |caller: Caller<WorldState>, h: u32| -> f32 {
            caller.data().input.axis1d(voxlconsl_types::ActionHandle(h))
        },
    )?;
    linker.func_wrap(
        "env", "input_action_axis2d",
        |mut caller: Caller<WorldState>, h: u32, out_x: u32, out_y: u32| {
            let (x, y) = caller.data().input.axis2d(voxlconsl_types::ActionHandle(h));
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return,
            };
            let _ = memory.write(&mut caller, out_x as usize, &x.to_le_bytes());
            let _ = memory.write(&mut caller, out_y as usize, &y.to_le_bytes());
        },
    )?;
    linker.func_wrap(
        "env", "input_action_active",
        |caller: Caller<WorldState>, h: u32| -> u32 {
            caller.data().input.is_active(voxlconsl_types::ActionHandle(h)) as u32
        },
    )?;

    // Misc (§8.3)
    linker.func_wrap(
        "env", "log",
        |caller: Caller<WorldState>, ptr: u32, len: u32| {
            // Read the cart's memory at [ptr, ptr+len) as UTF-8 and emit it.
            // Best-effort; ignore errors silently.
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return,
            };
            let mut buf = vec![0u8; len as usize];
            if memory.read(&caller, ptr as usize, &mut buf).is_err() {
                return;
            }
            // Best-effort UTF-8; just dump bytes if invalid.
            #[cfg(target_arch = "wasm32")]
            {
                let s = String::from_utf8_lossy(&buf);
                web_sys_log(&format!("[cart] {s}"));
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let s = String::from_utf8_lossy(&buf);
                eprintln!("[cart] {s}");
            }
        },
    )?;

    Ok(())
}

/// Decode the u32 the cart ABI passes for `Orientation`. Falls back to
/// `Up` for unknown values so a malformed cart can't crash the host.
/// The numbering matches the explicit `#[repr(u8)]` in
/// `voxlconsl-types::Orientation`.
fn orientation_from_u32(v: u32) -> Orientation {
    match v {
        0  => Orientation::Up,
        1  => Orientation::UpRot90,
        2  => Orientation::UpRot180,
        3  => Orientation::UpRot270,
        4  => Orientation::Down,
        5  => Orientation::DownRot90,
        6  => Orientation::DownRot180,
        7  => Orientation::DownRot270,
        8  => Orientation::EastUp,
        9  => Orientation::EastUpRot90,
        10 => Orientation::EastUpRot180,
        11 => Orientation::EastUpRot270,
        12 => Orientation::WestUp,
        13 => Orientation::WestUpRot90,
        14 => Orientation::WestUpRot180,
        15 => Orientation::WestUpRot270,
        16 => Orientation::NorthUp,
        17 => Orientation::NorthUpRot90,
        18 => Orientation::NorthUpRot180,
        19 => Orientation::NorthUpRot270,
        20 => Orientation::SouthUp,
        21 => Orientation::SouthUpRot90,
        22 => Orientation::SouthUpRot180,
        23 => Orientation::SouthUpRot270,
        _ => Orientation::Up,
    }
}

#[cfg(target_arch = "wasm32")]
fn web_sys_log(_msg: &str) {
    // The `voxlconsl-host` crate avoids depending on web-sys; the browser
    // host crate sets this up. For now, drop the message — TODO: thread
    // through via a host-provided log callback.
}
