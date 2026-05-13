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
    ActionKind, ActorId, ActorRenderMode, BindingHint, BodyId, BodyKind, Material, MaterialFlags,
    Orientation, PrefabId, Shape, ShapeTag, U8Vec3, Vec3,
    cart_format::{Cart as VoxlCart, CartError as VoxlError, MAGIC as VOXL_MAGIC, SectionId},
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
        let (wasm_bytes, materials_bytes, audio_bytes): (&[u8], Option<&[u8]>, Option<&[u8]>) =
            if bytes.len() >= VOXL_MAGIC.len() && bytes[..VOXL_MAGIC.len()] == VOXL_MAGIC {
                let cart = VoxlCart::parse(bytes)?;
                let code = cart.code();
                let materials = cart.section_bytes(SectionId::Materials);
                let audio = cart.section_bytes(SectionId::Audio);
                (code, materials, audio)
            } else {
                (bytes, None, None)
            };

        let engine = Engine::default();
        let module = Module::new(&engine, wasm_bytes)?;
        let mut world = WorldState::new();
        if let Some(mb) = materials_bytes {
            apply_materials_section(&mut world, mb);
        }
        if let Some(ab) = audio_bytes {
            apply_audio_section(&mut world, ab);
        }
        let mut store = Store::new(&engine, world);

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

/// Copy a `.voxl` Materials section payload into `world.materials`.
/// Silently ignores a malformed-size section (parser already validates
/// the section bounds; we just need the right multiple of 16 bytes).
///
/// We cast the *destination* slice to bytes rather than the source —
/// the .voxl byte slice is borrowed from `include_bytes!`/file memory
/// at u8 alignment, but `Material` has u16 fields so the cast direction
/// `&[u8] → &[Material]` can fail an alignment check. The destination
/// is heap-allocated and properly aligned, so `&mut [Material] → &mut
/// [u8]` always works.
fn apply_materials_section(world: &mut WorldState, bytes: &[u8]) {
    const EXPECTED: usize = 256 * core::mem::size_of::<Material>();
    if bytes.len() != EXPECTED {
        return;
    }
    let dst: &mut [u8] = bytemuck::cast_slice_mut(&mut world.materials[..]);
    dst.copy_from_slice(bytes);
}

/// Parse a `.voxl` Audio section payload and replay its entries into
/// the world's audio state + event log. Patches update the main-thread
/// shadow via `audio.patch_load` AND emit per-field events the worklet
/// will consume on the first drain. Samples + songs are event-only
/// (the shadow no longer stores either; the worklet is authoritative).
fn apply_audio_section(world: &mut WorldState, section: &[u8]) {
    use voxlconsl_audio::audio_section::{
        entries as iter_entries, KIND_PATCH, KIND_SAMPLE, KIND_SONG,
    };
    for entry in iter_entries(section) {
        match entry.kind {
            KIND_PATCH => apply_patch_entry(world, entry),
            KIND_SAMPLE => apply_sample_entry(world, entry),
            KIND_SONG => {
                world.audio_events.push_music_load(entry.slot, entry.data);
            }
            _ => {} // tolerate unknown kinds
        }
    }
}

fn apply_patch_entry(world: &mut WorldState, entry: voxlconsl_audio::audio_section::Entry<'_>) {
    // Update the main-thread shadow so `patch_save` reads the loaded
    // values; ignore failure (e.g. bad VPCH magic) — the shadow keeps
    // its default. Either way, replay events so the worklet stays in
    // sync with whatever the cart authored.
    let _ = world.audio.patch_load(entry.slot, entry.data);
    if let Some(patch) = voxlconsl_audio::patch_blob_load(entry.data) {
        world.audio_events.push_patch_full(entry.slot, &patch);
    }
}

fn apply_sample_entry(world: &mut WorldState, entry: voxlconsl_audio::audio_section::Entry<'_>) {
    let Some(view) = entry.as_sample() else { return };
    // Map the source rate to the engine's two-bucket SampleRate enum.
    // sample_load events use the same `rate_code` the cart-side
    // `SampleRate` enum encodes: 0 = 11.025 kHz, 1 = 22.05 kHz.
    let rate_code: u8 = if view.sample_rate_hz <= 11_025 { 0 } else { 1 };
    let (loop_start, loop_end, flags) = match view.loop_points {
        Some((s, e)) => (s, e, 1u8),
        None => (0, 0, 0),
    };
    world.audio_events.push_sample_load(
        entry.slot, rate_code, flags, loop_start, loop_end, view.pcm,
    );
}

/// Bring the active scene's chunk SVOs and the actor macro-grid back
/// in sync with cart-side mutations. Cheap when nothing changed.
fn prepare_for_queries(world: &mut WorldState) {
    world.flush();
    world.actors.flush_all();
    world.macro_grid.rebuild(&world.actors);
}

fn write_hit(
    caller: &mut Caller<WorldState>,
    out_ptr: u32,
    hit: Option<voxlconsl_types::Hit>,
) -> u32 {
    let hit = match hit { Some(h) => h, None => return 0 };
    let bytes = crate::physics::encode_hit(&hit);
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return 0,
    };
    let _ = memory.write(caller, out_ptr as usize, &bytes);
    1
}

fn write_sweep_hit(
    caller: &mut Caller<WorldState>,
    out_ptr: u32,
    hit: Option<voxlconsl_types::SweepHit>,
) -> u32 {
    let hit = match hit { Some(h) => h, None => return 0 };
    let bytes = crate::physics::encode_sweep_hit(&hit);
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return 0,
    };
    let _ = memory.write(caller, out_ptr as usize, &bytes);
    1
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
                ignites_to: 0,
                _reserved: [0; 8],
            };
            caller.data_mut().set_material(slot as u8, mat);
        },
    )?;

    // CA-tuning fields on an already-defined material (§2 / §10.3).
    // Lets carts set ca_threshold (granular angle-of-repose /
    // flammable ignition heat), ca_lifetime (gas / fire frames),
    // ca_viscosity (liquid flow rate), and ignites_to (flammable →
    // fire transformation target).
    linker.func_wrap(
        "env", "material_set_ca",
        |mut caller: Caller<WorldState>,
         slot: u32,
         threshold: u32, lifetime: u32, viscosity: u32, ignites_to: u32| {
            let world = caller.data_mut();
            let i = slot as usize & 0xFF;
            let mut mat = world.materials[i];
            mat.ca_threshold = threshold as u8;
            mat.ca_lifetime = lifetime as u8;
            mat.ca_viscosity = viscosity as u8;
            mat.ignites_to = ignites_to as u8;
            world.materials[i] = mat;
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
        "env", "viewport_set",
        |mut caller: Caller<WorldState>, x: u32, y: u32, w: u32, h: u32| {
            let world = caller.data_mut();
            // Clamp the rect against the framebuffer. A zero-width or
            // zero-height rect is allowed (renderer just skips the
            // ray-march entirely) — useful when the cart wants a UI-
            // only frame.
            let fb_w = crate::renderer::WIDTH;
            let fb_h = crate::renderer::HEIGHT;
            let x = x.min(fb_w);
            let y = y.min(fb_h);
            let w = w.min(fb_w.saturating_sub(x));
            let h = h.min(fb_h.saturating_sub(y));
            world.viewport = (x, y, w, h);
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
    linker.func_wrap(
        "env", "actor_set_render_mode",
        |mut caller: Caller<WorldState>, actor_id: u32, mode: u32| -> u32 {
            let m = match ActorRenderMode::from_code(mode) {
                Some(m) => m,
                None => return 0,    // unknown code → no-op, return false
            };
            let world = caller.data_mut();
            match world.actors.get_mut(ActorId(actor_id)) {
                Some(a) => { a.render_mode = m; 1 }
                None => 0,
            }
        },
    )?;
    linker.func_wrap(
        "env", "actor_get_render_mode",
        |caller: Caller<WorldState>, actor_id: u32| -> u32 {
            caller
                .data()
                .actors
                .get(ActorId(actor_id))
                .map(|a| a.render_mode as u32)
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
                _ => return u32::MAX,
            };
            // Must match the cart-side `BindingHint` discriminants in
            // voxlconsl-types. Don't skip Zoom — its slot (3) used to
            // be missing here, which silently shifted every subsequent
            // hint down by one: cart-side PrimaryFire (=4) was being
            // decoded as SecondaryFire (binding K), Zoom (=3) was
            // being decoded as PrimaryFire (binding J), etc. The
            // long-standing "J doesn't reach the cart, K does" symptom
            // was actually this off-by-one masquerade.
            let hint = match hint {
                0 => BindingHint::None,
                1 => BindingHint::PrimaryMovement,
                2 => BindingHint::Aim,
                3 => BindingHint::Zoom,
                4 => BindingHint::PrimaryFire,
                5 => BindingHint::SecondaryFire,
                6 => BindingHint::Confirm,
                7 => BindingHint::Cancel,
                8 => BindingHint::Menu,
                9 => BindingHint::Pause,
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
    // Writes the UTF-8 label for the binding currently driving `h`
    // into the cart-provided buffer, returning the byte count written
    // (capped at `cap`). See SPEC §6.5.
    linker.func_wrap(
        "env", "input_action_label",
        |mut caller: Caller<WorldState>,
         h: u32, out_ptr: u32, out_cap: u32| -> u32 {
            let label = caller.data().input.label(voxlconsl_types::ActionHandle(h));
            let bytes = label.as_bytes();
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };
            let n = bytes.len().min(out_cap as usize);
            if n > 0 {
                let _ = memory.write(&mut caller, out_ptr as usize, &bytes[..n]);
            }
            n as u32
        },
    )?;

    // Physics queries (§10.1)
    //
    // All six imports flush dirty chunk SVOs + the actor macro-grid
    // before answering, so cart code that mutates the world and
    // immediately queries it sees the post-write state. Flushes are
    // cheap when nothing changed (just walks dirty flags).
    linker.func_wrap(
        "env", "raycast",
        |mut caller: Caller<WorldState>,
         ox: f32, oy: f32, oz: f32,
         dx: f32, dy: f32, dz: f32,
         max_dist: f32,
         out_ptr: u32| -> u32 {
            prepare_for_queries(caller.data_mut());
            let hit = crate::physics::raycast(
                caller.data(),
                Vec3::new(ox, oy, oz),
                Vec3::new(dx, dy, dz),
                max_dist,
            );
            write_hit(&mut caller, out_ptr, hit)
        },
    )?;
    linker.func_wrap(
        "env", "raycast_world_only",
        |mut caller: Caller<WorldState>,
         ox: f32, oy: f32, oz: f32,
         dx: f32, dy: f32, dz: f32,
         max_dist: f32,
         out_ptr: u32| -> u32 {
            prepare_for_queries(caller.data_mut());
            let hit = crate::physics::raycast_world_only(
                caller.data(),
                Vec3::new(ox, oy, oz),
                Vec3::new(dx, dy, dz),
                max_dist,
            );
            write_hit(&mut caller, out_ptr, hit)
        },
    )?;
    linker.func_wrap(
        "env", "aabb_overlap_world",
        |mut caller: Caller<WorldState>,
         min_x: f32, min_y: f32, min_z: f32,
         max_x: f32, max_y: f32, max_z: f32| -> u32 {
            prepare_for_queries(caller.data_mut());
            crate::physics::aabb_overlap_world(
                caller.data(),
                Vec3::new(min_x, min_y, min_z),
                Vec3::new(max_x, max_y, max_z),
            ) as u32
        },
    )?;
    linker.func_wrap(
        "env", "aabb_overlap_actors",
        |mut caller: Caller<WorldState>,
         min_x: f32, min_y: f32, min_z: f32,
         max_x: f32, max_y: f32, max_z: f32| -> u64 {
            prepare_for_queries(caller.data_mut());
            crate::physics::aabb_overlap_actors(
                &caller.data().actors,
                Vec3::new(min_x, min_y, min_z),
                Vec3::new(max_x, max_y, max_z),
            ).0
        },
    )?;
    linker.func_wrap(
        "env", "sweep_aabb",
        |mut caller: Caller<WorldState>,
         min_x: f32, min_y: f32, min_z: f32,
         max_x: f32, max_y: f32, max_z: f32,
         mx: f32, my: f32, mz: f32,
         out_ptr: u32| -> u32 {
            prepare_for_queries(caller.data_mut());
            let hit = crate::physics::sweep_aabb(
                caller.data(),
                Vec3::new(min_x, min_y, min_z),
                Vec3::new(max_x, max_y, max_z),
                Vec3::new(mx, my, mz),
            );
            write_sweep_hit(&mut caller, out_ptr, hit)
        },
    )?;
    linker.func_wrap(
        "env", "material_at",
        |mut caller: Caller<WorldState>, x: u32, y: u32, z: u32| -> u32 {
            prepare_for_queries(caller.data_mut());
            crate::physics::material_at(caller.data(), x, y, z) as u32
        },
    )?;

    // Bodies (§10.2) — Layer 2 rigid bodies. Cart spawns AABB / sphere
    // bodies attached to actors; the host integrates them each frame.
    //
    // `body_spawn(actor, kind, shape_tag, sx, sy, sz, mass)` returns a
    // BodyId or u32::MAX on cap exhaustion. `actor` of u32::MAX leaves
    // the body unattached. Shape: tag 0 = AABB (sx, sy, sz = full
    // extents), tag 1 = Sphere (sx = radius, sy/sz ignored).
    linker.func_wrap(
        "env", "body_spawn",
        |mut caller: Caller<WorldState>,
         actor: u32, kind: u32, shape_tag: u32,
         sx: f32, sy: f32, sz: f32,
         mass: f32| -> u32 {
            let kind = BodyKind::from_u8(kind as u8);
            let shape = Shape::from_parts(ShapeTag::from_u8(shape_tag as u8), [sx, sy, sz]);
            let actor_opt = if actor == u32::MAX { None } else { Some(ActorId(actor)) };
            let world = caller.data_mut();
            let body = crate::bodies::Body {
                kind,
                shape,
                position: actor_opt
                    .and_then(|a| world.actors.get(a))
                    .map(|a| a.position + shape.half_extents())
                    .unwrap_or(Vec3::ZERO),
                velocity: Vec3::ZERO,
                mass: if mass > 0.0 { mass } else { 1.0 },
                restitution: 0.0,
                friction: 0.0,
                layer: 0,
                mask: 0xFF,
                sensor: false,
                actor: actor_opt,
            };
            world.bodies.spawn(body).map(|id| id.0).unwrap_or(u32::MAX)
        },
    )?;
    linker.func_wrap(
        "env", "body_despawn",
        |mut caller: Caller<WorldState>, id: u32| {
            caller.data_mut().bodies.despawn(BodyId(id));
        },
    )?;
    linker.func_wrap(
        "env", "body_set_kind",
        |mut caller: Caller<WorldState>, id: u32, kind: u32| {
            if let Some(b) = caller.data_mut().bodies.get_mut(BodyId(id)) {
                b.kind = BodyKind::from_u8(kind as u8);
            }
        },
    )?;
    linker.func_wrap(
        "env", "body_set_position",
        |mut caller: Caller<WorldState>, id: u32, x: f32, y: f32, z: f32| {
            if let Some(b) = caller.data_mut().bodies.get_mut(BodyId(id)) {
                b.position = Vec3::new(x, y, z);
            }
        },
    )?;
    linker.func_wrap(
        "env", "body_set_velocity",
        |mut caller: Caller<WorldState>, id: u32, x: f32, y: f32, z: f32| {
            if let Some(b) = caller.data_mut().bodies.get_mut(BodyId(id)) {
                b.velocity = Vec3::new(x, y, z);
            }
        },
    )?;
    linker.func_wrap(
        "env", "body_apply_impulse",
        |mut caller: Caller<WorldState>, id: u32, jx: f32, jy: f32, jz: f32| {
            if let Some(b) = caller.data_mut().bodies.get_mut(BodyId(id)) {
                let inv_m = if matches!(b.kind, BodyKind::Dynamic) && b.mass > 0.0 {
                    1.0 / b.mass
                } else {
                    0.0
                };
                b.velocity = b.velocity + Vec3::new(jx, jy, jz) * inv_m;
            }
        },
    )?;
    linker.func_wrap(
        "env", "body_set_layer",
        |mut caller: Caller<WorldState>, id: u32, layer: u32, mask: u32| {
            if let Some(b) = caller.data_mut().bodies.get_mut(BodyId(id)) {
                b.layer = (layer as u8) & 0x07;
                b.mask = mask as u8;
            }
        },
    )?;
    linker.func_wrap(
        "env", "body_set_sensor",
        |mut caller: Caller<WorldState>, id: u32, sensor: u32| {
            if let Some(b) = caller.data_mut().bodies.get_mut(BodyId(id)) {
                b.sensor = sensor != 0;
            }
        },
    )?;
    linker.func_wrap(
        "env", "body_set_material",
        |mut caller: Caller<WorldState>, id: u32, restitution: f32, friction: f32| {
            if let Some(b) = caller.data_mut().bodies.get_mut(BodyId(id)) {
                b.restitution = restitution.clamp(0.0, 1.0);
                b.friction = friction.clamp(0.0, 1.0);
            }
        },
    )?;
    linker.func_wrap(
        "env", "body_get",
        |mut caller: Caller<WorldState>, id: u32, out_ptr: u32| -> u32 {
            let snap = match caller.data().bodies.get(BodyId(id)) {
                Some(b) => b.snapshot(),
                None => return 0,
            };
            let bytes = crate::bodies::encode_body_state(&snap);
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };
            let _ = memory.write(&mut caller, out_ptr as usize, &bytes);
            1
        },
    )?;
    linker.func_wrap(
        "env", "world_set_gravity",
        |mut caller: Caller<WorldState>, gx: f32, gy: f32, gz: f32| {
            caller.data_mut().bodies.gravity = Vec3::new(gx, gy, gz);
        },
    )?;
    linker.func_wrap(
        "env", "drain_collision_events",
        |mut caller: Caller<WorldState>, buf_ptr: u32, max: u32| -> u32 {
            let max = max as usize;
            // Pull out up to `max` events, encode each in 36 bytes, write.
            let events: Vec<voxlconsl_types::CollisionEvent> = {
                let table = &mut caller.data_mut().bodies;
                let n = table.events.len().min(max);
                table.events.drain(0..n).collect()
            };
            if events.is_empty() {
                return 0;
            }
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };
            for (i, ev) in events.iter().enumerate() {
                let bytes = crate::bodies::encode_collision_event(ev);
                let off = buf_ptr as usize + i * 36;
                if memory.write(&mut caller, off, &bytes).is_err() {
                    return i as u32;
                }
            }
            events.len() as u32
        },
    )?;

    // CA (§10.3) — sparse cellular-automata sim for granular / liquid /
    // gas / flammable / fire materials. v0.1.x ships granular fully;
    // other rules dispatch through the same active set but no-op.
    linker.func_wrap(
        "env", "ca_set_budget",
        |mut caller: Caller<WorldState>, voxels_per_frame: u32| {
            caller.data_mut().ca.budget = voxels_per_frame;
        },
    )?;
    linker.func_wrap(
        "env", "ca_get_budget",
        |caller: Caller<WorldState>| -> u32 {
            caller.data().ca.budget
        },
    )?;
    linker.func_wrap(
        "env", "ca_mark_active",
        |mut caller: Caller<WorldState>, x: u32, y: u32, z: u32| {
            caller.data_mut().ca.mark_active(x, y, z);
        },
    )?;
    linker.func_wrap(
        "env", "ca_active_count",
        |caller: Caller<WorldState>| -> u32 {
            caller.data().ca.active_count() as u32
        },
    )?;
    linker.func_wrap(
        "env", "ca_set_global_param",
        |_caller: Caller<WorldState>, _param: u32, _value: f32| {
            // Reserved for v2 — no global params tunable in v1's
            // granular-only sim.
        },
    )?;

    // Audio (§5). Every cart-facing audio import does two things:
    //   1. Update the main-thread "shadow" `AudioState` so synchronous
    //      reads (patch_save, etc.) stay current.
    //   2. Append the event to `audio_events`. The browser-host shim
    //      drains that log after each cart frame and relays the bytes
    //      to the AudioWorkletProcessor, where the authoritative
    //      mixer runs (SPEC.md §5.8, Stage 4b Phase 2c+).
    linker.func_wrap(
        "env", "sample_load",
        |mut caller: Caller<WorldState>,
         slot: u32, ptr: u32, len: u32,
         rate_code: u32, flags: u32,
         loop_start: u32, loop_end: u32| -> u32 {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };
            let mut buf = vec![0u8; len as usize];
            if memory.read(&caller, ptr as usize, &mut buf).is_err() {
                return 0;
            }
            // No shadow update: only the worklet's mixer reads sample
            // bytes (cart-side `sample_register` always returns ok
            // before this; the worklet may reject the slot later).
            caller.data_mut().audio_events.push_sample_load(
                slot as u8, rate_code as u8, flags as u8,
                loop_start, loop_end, &buf,
            );
            1
        },
    )?;
    linker.func_wrap(
        "env", "sfx_play",
        |mut caller: Caller<WorldState>,
         slot: u32, volume: u32, pan: i32, pitch_cents: i32, loop_: u32| -> u32 {
            let world = caller.data_mut();
            let token = world.alloc_voice_token();
            world.audio_events.push_sfx_play(
                token, slot as u8, volume as u8, pan as i8, pitch_cents as i16, loop_ != 0,
            );
            token
        },
    )?;
    linker.func_wrap(
        "env", "sfx_stop",
        |mut caller: Caller<WorldState>, voice: u32| {
            caller.data_mut().audio_events.push_sfx_stop(voice);
        },
    )?;
    linker.func_wrap(
        "env", "sfx_set_volume",
        |mut caller: Caller<WorldState>, voice: u32, volume: u32| {
            caller.data_mut().audio_events.push_sfx_set_volume(voice, volume as u8);
        },
    )?;
    linker.func_wrap(
        "env", "sfx_set_pitch",
        |mut caller: Caller<WorldState>, voice: u32, pitch_cents: i32| {
            caller.data_mut().audio_events.push_sfx_set_pitch(voice, pitch_cents as i16);
        },
    )?;

    // Stage 2 — synth patches + voice trigger / release.
    linker.func_wrap(
        "env", "patch_set_osc",
        |mut caller: Caller<WorldState>,
         slot: u32, osc_idx: u32,
         mode: u32, detune_cents: i32, octave: i32, level: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_osc(
                slot as u8, osc_idx as u8, mode as u8,
                detune_cents as i16, octave as i8, level as u8,
            );
            world.audio.patch_set_osc(
                slot as u8, osc_idx as u8,
                crate::audio::OscMode::from_code(mode as u8),
                detune_cents as i16, octave as i8, level as u8,
            );
        },
    )?;
    linker.func_wrap(
        "env", "patch_set_filter",
        |mut caller: Caller<WorldState>,
         slot: u32, mode: u32, cutoff_hz: u32, resonance: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_filter(
                slot as u8, mode as u8, cutoff_hz as u16, resonance as u8,
            );
            world.audio.patch_set_filter(
                slot as u8,
                crate::audio::FilterMode::from_code(mode as u8),
                cutoff_hz as u16, resonance as u8,
            );
        },
    )?;
    linker.func_wrap(
        "env", "patch_set_amp_env",
        |mut caller: Caller<WorldState>,
         slot: u32, attack_ms: u32, decay_ms: u32, sustain: u32, release_ms: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_amp_env(
                slot as u8, attack_ms as u16, decay_ms as u16, sustain as u8, release_ms as u16,
            );
            world.audio.patch_set_amp_env(
                slot as u8, attack_ms as u16, decay_ms as u16, sustain as u8, release_ms as u16,
            );
        },
    )?;
    linker.func_wrap(
        "env", "patch_set_filter_env",
        |mut caller: Caller<WorldState>,
         slot: u32, attack_ms: u32, decay_ms: u32, sustain: u32, release_ms: u32, depth: i32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_filter_env(
                slot as u8, attack_ms as u16, decay_ms as u16,
                sustain as u8, release_ms as u16, depth as i8,
            );
            world.audio.patch_set_filter_env(
                slot as u8, attack_ms as u16, decay_ms as u16,
                sustain as u8, release_ms as u16, depth as i8,
            );
        },
    )?;
    linker.func_wrap(
        "env", "patch_set_lfo",
        |mut caller: Caller<WorldState>,
         slot: u32, rate_centihz: u32, shape: u32, target: u32, depth: i32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_lfo(
                slot as u8, rate_centihz as u16, shape as u8, target as u8, depth as i8,
            );
            world.audio.patch_set_lfo(
                slot as u8, rate_centihz as u16,
                crate::audio::LfoShape::from_code(shape as u8),
                crate::audio::LfoTarget::from_code(target as u8),
                depth as i8,
            );
        },
    )?;
    linker.func_wrap(
        "env", "patch_set_glide",
        |mut caller: Caller<WorldState>, slot: u32, ms: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_glide(slot as u8, ms as u16);
            world.audio.patch_set_glide(slot as u8, ms as u16);
        },
    )?;
    linker.func_wrap(
        "env", "patch_set_fm",
        |mut caller: Caller<WorldState>, slot: u32, ratio_q88: u32, index_q88: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_fm(slot as u8, ratio_q88 as u16, index_q88 as u16);
            world.audio.patch_set_fm(slot as u8, ratio_q88 as u16, index_q88 as u16);
        },
    )?;
    linker.func_wrap(
        "env", "patch_set_kind",
        |mut caller: Caller<WorldState>, slot: u32, kind_code: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_kind(slot as u8, kind_code as u8);
            world.audio.patch_set_kind(
                slot as u8,
                crate::audio::PatchKind::from_code(kind_code as u8),
            );
        },
    )?;
    linker.func_wrap(
        "env", "patch_set_zone_count",
        |mut caller: Caller<WorldState>, slot: u32, count: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_zone_count(slot as u8, count as u8);
            world.audio.patch_set_zone_count(slot as u8, count as u8);
        },
    )?;
    linker.func_wrap(
        "env", "patch_save",
        |mut caller: Caller<WorldState>, slot: u32, ptr: u32, max_len: u32| -> u32 {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };
            let mut buf = vec![0u8; max_len as usize];
            let n = caller.data().audio.patch_save(slot as u8, &mut buf);
            if n == 0 || n as usize > buf.len() {
                return 0;
            }
            if memory.write(&mut caller, ptr as usize, &buf[..n as usize]).is_err() {
                return 0;
            }
            n
        },
    )?;
    linker.func_wrap(
        "env", "patch_load",
        |mut caller: Caller<WorldState>, slot: u32, ptr: u32, len: u32| -> u32 {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };
            let mut buf = vec![0u8; len as usize];
            if memory.read(&caller, ptr as usize, &mut buf).is_err() {
                return 0;
            }
            if caller.data_mut().audio.patch_load(slot as u8, &buf) {
                1
            } else {
                0
            }
        },
    )?;
    linker.func_wrap(
        "env", "patch_set_zone",
        |mut caller: Caller<WorldState>,
         slot: u32, zone_idx: u32,
         low_note: u32, high_note: u32, root_note: u32,
         sample_slot: u32, volume_offset: i32,
         loop_start: u32, loop_end: u32, loop_enabled: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_set_zone(
                slot as u8, zone_idx as u8,
                low_note as u8, high_note as u8, root_note as u8,
                sample_slot as u8, volume_offset as i8,
                loop_start, loop_end, loop_enabled != 0,
            );
            world.audio.patch_set_zone(
                slot as u8, zone_idx as u8,
                crate::audio::KeyZone {
                    low_note: low_note as u8,
                    high_note: high_note as u8,
                    root_note: root_note as u8,
                    sample_slot: sample_slot as u8,
                    volume_offset: volume_offset as i8,
                    loop_start, loop_end,
                    loop_enabled: loop_enabled != 0,
                },
            );
        },
    )?;
    linker.func_wrap(
        "env", "patch_reset",
        |mut caller: Caller<WorldState>, slot: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_reset(slot as u8);
            world.audio.patch_reset(slot as u8);
        },
    )?;
    linker.func_wrap(
        "env", "patch_copy",
        |mut caller: Caller<WorldState>, src: u32, dst: u32| {
            let world = caller.data_mut();
            world.audio_events.push_patch_copy(src as u8, dst as u8);
            world.audio.patch_copy(src as u8, dst as u8);
        },
    )?;
    linker.func_wrap(
        "env", "voice_trigger",
        |mut caller: Caller<WorldState>, patch: u32, note: u32, velocity: u32| -> u32 {
            let world = caller.data_mut();
            let token = world.alloc_voice_token();
            world.audio_events.push_voice_trigger(token, patch as u8, note as u8, velocity as u8);
            token
        },
    )?;
    linker.func_wrap(
        "env", "voice_release",
        |mut caller: Caller<WorldState>, voice: u32| {
            caller.data_mut().audio_events.push_voice_release(voice);
        },
    )?;

    // Stage 3 — MIDI event surface (§5.2).
    linker.func_wrap(
        "env", "note_on",
        |mut caller: Caller<WorldState>, channel: u32, note: u32, velocity: u32| -> u32 {
            let world = caller.data_mut();
            let token = world.alloc_voice_token();
            world.audio_events.push_note_on(token, channel as u8, note as u8, velocity as u8);
            token
        },
    )?;
    linker.func_wrap(
        "env", "note_off",
        |mut caller: Caller<WorldState>, channel: u32, note: u32| {
            caller.data_mut().audio_events.push_note_off(channel as u8, note as u8);
        },
    )?;
    linker.func_wrap(
        "env", "pitch_bend",
        |mut caller: Caller<WorldState>, channel: u32, value: i32| {
            caller.data_mut().audio_events.push_pitch_bend(channel as u8, value as i16);
        },
    )?;
    linker.func_wrap(
        "env", "cc",
        |mut caller: Caller<WorldState>, channel: u32, controller: u32, value: u32| {
            caller.data_mut().audio_events.push_cc(channel as u8, controller as u8, value as u8);
        },
    )?;
    linker.func_wrap(
        "env", "program_change",
        |mut caller: Caller<WorldState>, channel: u32, patch: u32| {
            caller.data_mut().audio_events.push_program_change(channel as u8, patch as u8);
        },
    )?;
    linker.func_wrap(
        "env", "all_notes_off",
        |mut caller: Caller<WorldState>, channel: u32| {
            caller.data_mut().audio_events.push_all_notes_off(channel as u8);
        },
    )?;

    // Stage 4a — SMF song playback (§5.3).
    linker.func_wrap(
        "env", "music_load",
        |mut caller: Caller<WorldState>, slot: u32, ptr: u32, len: u32| -> u32 {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };
            let mut buf = vec![0u8; len as usize];
            if memory.read(&caller, ptr as usize, &mut buf).is_err() {
                return 0;
            }
            // Best-effort parse-validation on main so cart's return
            // value still reflects whether the bytes are syntactically
            // a valid SMF (mismatched MThd, format-2, etc.). Worklet
            // does its own parse for the playback state.
            let ok = crate::audio::parse_smf(&buf).is_ok();
            caller.data_mut().audio_events.push_music_load(slot as u8, &buf);
            if ok { 1 } else { 0 }
        },
    )?;
    linker.func_wrap(
        "env", "music_play",
        |mut caller: Caller<WorldState>, slot: u32, loop_: u32| {
            caller.data_mut().audio_events.push_music_play(slot as u8, loop_ != 0);
        },
    )?;
    linker.func_wrap(
        "env", "music_stop",
        |mut caller: Caller<WorldState>| {
            caller.data_mut().audio_events.push_music_stop();
        },
    )?;
    linker.func_wrap(
        "env", "music_set_tempo_scale",
        |mut caller: Caller<WorldState>, scale: f32| {
            caller.data_mut().audio_events.push_music_set_tempo_scale(scale);
        },
    )?;
    linker.func_wrap(
        "env", "music_position_beats",
        |caller: Caller<WorldState>| -> f32 {
            // Authoritative value lives on the worklet thread; main
            // reads the cached mirror updated by JS after each
            // worklet `state` post.
            caller.data().audio_music_beats_cached
        },
    )?;

    // Stage 5 — global FX bus (§5.5).
    linker.func_wrap(
        "env", "reverb_set",
        |mut caller: Caller<WorldState>, room_size: u32, damping: u32| {
            caller.data_mut().audio_events.push_reverb_set(room_size as u8, damping as u8);
        },
    )?;
    linker.func_wrap(
        "env", "delay_set",
        |mut caller: Caller<WorldState>, time_ms: u32, feedback: u32| {
            caller.data_mut().audio_events.push_delay_set(time_ms as u16, feedback as u8);
        },
    )?;

    // Misc (§8.3)
    //
    // Carts import this as `env.host_log` rather than `env.log` to
    // dodge a wasm-ld symbol collision with the math `log(f64)->f64`
    // pulled in transitively by `core::fmt` (see the SDK's
    // `pub fn log` decl). The Rust-side callable stays `log()`. We
    // also keep `env.log` registered for backwards compat with any
    // already-shipped cart wasm whose import name predates the rename;
    // wasmi silently ignores host-side imports that the module doesn't
    // declare, so the duplicate registration is harmless.
    let log_handler = |caller: Caller<WorldState>, ptr: u32, len: u32| {
        let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
            Some(m) => m,
            None => return,
        };
        let mut buf = vec![0u8; len as usize];
        if memory.read(&caller, ptr as usize, &mut buf).is_err() {
            return;
        }
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
    };
    linker.func_wrap("env", "host_log", log_handler)?;
    linker.func_wrap("env", "log",      log_handler)?;

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

/// Host-provided log callback. `voxlconsl-host` doesn't depend on
/// `web-sys` directly (so the crate stays buildable for native CLI /
/// tests / MCU targets); the embedding host crate (host-browser etc.)
/// installs this at startup via `set_log_callback`. When unset, cart
/// log output is silently dropped.
///
/// Single-threaded wasm32 runtime — plain `static mut` is fine. Native
/// hosts that use this for testing should call `set_log_callback`
/// before loading any cart.
static mut LOG_CALLBACK: Option<fn(&str)> = None;

pub fn set_log_callback(cb: fn(&str)) {
    unsafe { LOG_CALLBACK = Some(cb); }
}

#[cfg(target_arch = "wasm32")]
fn web_sys_log(msg: &str) {
    unsafe {
        if let Some(cb) = LOG_CALLBACK {
            cb(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_materials_section_overwrites_world_materials() {
        let mut world = WorldState::new();
        // Build a 256 × 16 Material array with slot 3 carrying a known
        // signature so we can confirm the copy went through.
        let mut table = vec![Material::AIR; 256];
        table[3] = Material {
            color: Material::pack_color(11, 2),
            emission: 7,
            flags: MaterialFlags::empty().with(MaterialFlags::FLAMMABLE),
            ca_threshold: 90,
            ca_lifetime: 0,
            ca_viscosity: 0,
            ignites_to: 13,
            _reserved: [0; 8],
        };
        // table is already a Vec<Material> with proper alignment.
        let bytes: &[u8] = bytemuck::cast_slice(&table[..]);
        apply_materials_section(&mut world, bytes);
        assert_eq!(world.materials[3].color, Material::pack_color(11, 2));
        assert_eq!(world.materials[3].emission, 7);
        assert!(world.materials[3].flags.contains(MaterialFlags::FLAMMABLE));
        assert_eq!(world.materials[3].ca_threshold, 90);
        assert_eq!(world.materials[3].ignites_to, 13);
    }

    #[test]
    fn apply_audio_section_replays_patches_and_assets() {
        use voxlconsl_audio::audio_section::{
            ENTRY_SIZE, HEADER_SIZE, KIND_PATCH, KIND_SAMPLE, KIND_SONG,
            MAGIC as AUDIO_MAGIC, NO_LOOP, SAMPLE_PREAMBLE_SIZE, VERSION as AUDIO_VERSION,
        };

        // Hand-built minimal section with one patch + one sample + one
        // song. The patch payload is a VPCH blob with patch_load=true
        // semantics so the shadow gets a known osc[0].mode.
        let mut patch = voxlconsl_audio::Patch::default_synth();
        patch.osc[0].mode = voxlconsl_audio::OscMode::Saw;
        patch.osc[0].level = 99;
        let mut patch_blob = [0u8; voxlconsl_audio::PATCH_BLOB_MAX];
        let n = voxlconsl_audio::patch_blob_save(&patch, &mut patch_blob) as usize;
        let patch_bytes = patch_blob[..n].to_vec();

        let mut sample_payload = Vec::with_capacity(SAMPLE_PREAMBLE_SIZE + 4);
        sample_payload.extend_from_slice(&22_050u32.to_le_bytes());
        sample_payload.extend_from_slice(&NO_LOOP.to_le_bytes());
        sample_payload.extend_from_slice(&0u32.to_le_bytes());
        sample_payload.extend_from_slice(&[100u8, 110, 120, 130]);

        let song_payload = b"MThd-stub".to_vec();

        let payloads: [&[u8]; 3] = [&patch_bytes, &sample_payload, &song_payload];
        let kinds = [KIND_PATCH, KIND_SAMPLE, KIND_SONG];
        let slots = [7u8, 3, 1];
        let mut section =
            vec![0u8; HEADER_SIZE + payloads.len() * ENTRY_SIZE];
        section[0..4].copy_from_slice(&AUDIO_MAGIC);
        section[4] = AUDIO_VERSION;
        section[6..8].copy_from_slice(&(payloads.len() as u16).to_le_bytes());
        let mut offsets = Vec::with_capacity(3);
        for p in &payloads {
            offsets.push(section.len());
            section.extend_from_slice(p);
        }
        for i in 0..3 {
            let at = HEADER_SIZE + i * ENTRY_SIZE;
            section[at] = kinds[i];
            section[at + 1] = slots[i];
            section[at + 4..at + 8]
                .copy_from_slice(&(offsets[i] as u32).to_le_bytes());
            section[at + 8..at + 12]
                .copy_from_slice(&(payloads[i].len() as u32).to_le_bytes());
        }

        let mut world = WorldState::new();
        apply_audio_section(&mut world, &section);

        // Shadow received the patch (so `patch_save` would read it back).
        let mut readback = [0u8; voxlconsl_audio::PATCH_BLOB_MAX];
        let n2 = world.audio.patch_save(7, &mut readback);
        assert!(n2 > 0);
        let parsed = voxlconsl_audio::patch_blob_load(&readback[..n2 as usize]).unwrap();
        assert_eq!(parsed.osc[0].mode, voxlconsl_audio::OscMode::Saw);
        assert_eq!(parsed.osc[0].level, 99);

        // Event log received a sample_load + music_load + patch field events.
        assert!(!world.audio_events.buf.is_empty());
    }

    #[test]
    fn apply_materials_section_ignores_wrong_length() {
        let mut world = WorldState::new();
        // Seed slot 5 with a value that should survive the ignored apply.
        world.materials[5] = Material {
            color: 42,
            ..Material::AIR
        };
        apply_materials_section(&mut world, &[0u8; 10]); // way too short
        assert_eq!(world.materials[5].color, 42);
    }
}
