//! Layer 2 rigid bodies (§10.2).
//!
//! AABB / sphere bodies attached to actors. The host:
//!   1. applies gravity to Dynamic bodies,
//!   2. integrates each body's position along its velocity using a
//!      fixed sub-step (1/60 s, up to 4 sub-steps per frame), with
//!      axis-separated swept-AABB collision against world voxels,
//!   3. resolves pairwise body-vs-body contacts with layer/mask
//!      filtering, and
//!   4. synchronizes `body.position → actor.position` for any body
//!      with an attached actor.
//!
//! Bodies never mutate the world grid; voxel destruction stays a cart
//! responsibility per §10.2 ("Bodies do not mutate the world grid").
//!
//! Caps are 64 active bodies and 256 queued collision events per the
//! spec. Pair detection is O(n²) over visible bodies; with `n ≤ 64`
//! that's a few thousand cheap layer-mask checks per frame.
//!
//! Sphere shapes are simulated using their **bounding AABB** for
//! world-vs-body collision (voxel sweep is grid-aligned and box-shaped
//! anyway), and proper Minkowski math for sphere-vs-sphere and
//! sphere-vs-AABB pair contacts.

use voxlconsl_types::{
    ActorId, BodyId, BodyKind, BodyState, CollisionEvent, Shape, Vec3,
};

use crate::world::{WorldState, WORLD_SIDE};

/// Max simultaneously-live bodies per cart (§10.2 "Caps").
pub const MAX_BODIES: usize = 64;
/// Max queued collision events per tick. Overflow drops oldest.
pub const MAX_EVENTS: usize = 256;
/// Fixed physics sub-step (~16.67 ms — 60 Hz).
pub const FIXED_DT: f32 = 1.0 / 60.0;
/// Max sub-steps a single frame is allowed to consume. Beyond this we
/// drop accumulated time so a slow host frame doesn't snowball into a
/// physics-burns-the-CPU spiral.
pub const MAX_SUBSTEPS: u32 = 4;
/// Velocities below this magnitude are clamped to zero on contact so
/// resting bodies don't jitter from float drift.
const REST_VELOCITY_EPS: f32 = 0.005;
/// Skin for axis sweeps — keeps a body strictly outside contact so
/// follow-up overlap probes don't immediately re-collide.
const SKIN: f32 = 1e-4;

#[derive(Copy, Clone, Debug)]
pub struct Body {
    pub kind: BodyKind,
    pub shape: Shape,
    pub position: Vec3,
    pub velocity: Vec3,
    pub mass: f32,
    pub restitution: f32,
    pub friction: f32,
    pub layer: u8,
    pub mask: u8,
    pub sensor: bool,
    /// `None` if the body is unattached from any actor.
    pub actor: Option<ActorId>,
}

impl Body {
    /// World-space AABB inferred from `position` and shape half-extents.
    pub fn aabb(&self) -> (Vec3, Vec3) {
        let he = self.shape.half_extents();
        (self.position - he, self.position + he)
    }

    /// Helpful for `body_get` — produces the wire-form snapshot.
    pub fn snapshot(&self) -> BodyState {
        BodyState {
            kind: self.kind as u8,
            shape_tag: self.shape.tag() as u8,
            layer: self.layer,
            mask: self.mask,
            sensor: self.sensor as u8,
            _pad: [0; 3],
            shape: self.shape.to_floats(),
            position: self.position,
            velocity: self.velocity,
            mass: self.mass,
            restitution: self.restitution,
            friction: self.friction,
            actor: self.actor.map(|a| a.0).unwrap_or(BodyState::NO_ACTOR),
        }
    }
}

/// Slot table for bodies. `BodyId.0` is a slot index; freed slots are
/// reused. Counts hold to the per-cart cap.
pub struct BodyTable {
    slots: Vec<Option<Body>>,
    free: Vec<u32>,
    live_count: u32,
    /// Tunable gravity applied each sub-step to Dynamic bodies.
    /// Defaults to zero so a cart that hasn't opted in stays floaty.
    pub gravity: Vec3,
    /// Pending collision events. The cart drains via
    /// `drain_collision_events`. Capped at `MAX_EVENTS`; overflow drops
    /// the oldest entries so the cart still sees recent contacts even
    /// in a busy frame.
    pub events: Vec<CollisionEvent>,
    /// Accumulator for fixed sub-steps when frame dt isn't an exact
    /// multiple of `FIXED_DT`.
    pub accumulator_s: f32,
    /// Dropped-events counter exposed for telemetry.
    pub events_dropped: u64,
}

impl BodyTable {
    pub fn new() -> Self {
        Self {
            slots: Vec::with_capacity(MAX_BODIES),
            free: Vec::with_capacity(MAX_BODIES),
            live_count: 0,
            gravity: Vec3::ZERO,
            events: Vec::with_capacity(MAX_EVENTS),
            accumulator_s: 0.0,
            events_dropped: 0,
        }
    }

    pub fn spawn(&mut self, body: Body) -> Option<BodyId> {
        if self.live_count as usize >= MAX_BODIES {
            return None;
        }
        let idx = if let Some(i) = self.free.pop() {
            self.slots[i as usize] = Some(body);
            i
        } else {
            let i = self.slots.len() as u32;
            self.slots.push(Some(body));
            i
        };
        self.live_count += 1;
        Some(BodyId(idx))
    }

    pub fn despawn(&mut self, id: BodyId) {
        let i = id.0 as usize;
        if let Some(slot) = self.slots.get_mut(i) {
            if slot.is_some() {
                *slot = None;
                self.free.push(id.0);
                self.live_count = self.live_count.saturating_sub(1);
            }
        }
    }

    pub fn get(&self, id: BodyId) -> Option<&Body> {
        self.slots.get(id.0 as usize).and_then(|s| s.as_ref())
    }

    pub fn get_mut(&mut self, id: BodyId) -> Option<&mut Body> {
        self.slots.get_mut(id.0 as usize).and_then(|s| s.as_mut())
    }

    pub fn count(&self) -> u32 { self.live_count }

    pub fn capacity_slots(&self) -> usize { self.slots.len() }

    /// Iterate `(slot_index, &Body)` over every live body, ordered by
    /// slot index. Used by the integrator and pair loop.
    fn iter_with_index(&self) -> impl Iterator<Item = (u32, &Body)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|b| (i as u32, b)))
    }

    /// Push a collision event, dropping the oldest entry if full.
    fn push_event(&mut self, ev: CollisionEvent) {
        if self.events.len() >= MAX_EVENTS {
            // Drop the oldest. `Vec::remove(0)` is O(n) but the queue
            // is bounded at 256, and overflow is the unusual case.
            self.events.remove(0);
            self.events_dropped = self.events_dropped.saturating_add(1);
        }
        self.events.push(ev);
    }

    /// Drain up to `max` events into `buf`. Returns the number of events
    /// actually drained. Remaining events stay queued for the next call.
    pub fn drain_events(&mut self, buf: &mut [CollisionEvent], max: usize) -> usize {
        let n = self.events.len().min(max).min(buf.len());
        for i in 0..n {
            buf[i] = self.events[i];
        }
        self.events.drain(0..n);
        n
    }
}

impl Default for BodyTable {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// Integrator
// ============================================================================

/// Advance the body simulation by `frame_dt_s` seconds. Runs as many
/// `FIXED_DT` sub-steps as fit, up to `MAX_SUBSTEPS`. Excess frame time
/// accumulates in `bodies.accumulator_s` so a 30 Hz frame still
/// produces 60 Hz physics in expectation.
pub fn step(world: &mut WorldState, frame_dt_s: f32) {
    if frame_dt_s <= 0.0 {
        return;
    }
    world.bodies.accumulator_s += frame_dt_s;
    let mut steps = 0u32;
    while world.bodies.accumulator_s >= FIXED_DT && steps < MAX_SUBSTEPS {
        substep(world, FIXED_DT);
        world.bodies.accumulator_s -= FIXED_DT;
        steps += 1;
    }
    if steps >= MAX_SUBSTEPS {
        // Drop any further accumulated time so a slow host can't push
        // physics into an arbitrarily long catch-up.
        world.bodies.accumulator_s = 0.0;
    }
    sync_actors(world);
}

fn substep(world: &mut WorldState, dt: f32) {
    // Snapshot slot indices we'll integrate. We iterate by index so we
    // can mutate bodies one at a time without holding a long borrow.
    let n = world.bodies.capacity_slots();

    // Step 1 — gravity + per-body world-collision integration.
    for i in 0..n {
        let Some(body) = world.bodies.slots[i].as_ref() else { continue };
        if body.kind != BodyKind::Dynamic {
            continue;
        }
        let body_id = BodyId(i as u32);
        integrate_dynamic(world, body_id, dt);
    }

    // Step 2 — pairwise body-vs-body collisions.
    resolve_pairs(world);
}

/// Integrate one Dynamic body: apply gravity, then move along
/// velocity, axis-separated, clipping against the voxel grid.
fn integrate_dynamic(world: &mut WorldState, id: BodyId, dt: f32) {
    let (gravity, mut body) = {
        let g = world.bodies.gravity;
        let b = match world.bodies.slots[id.0 as usize] {
            Some(b) => b,
            None => return,
        };
        (g, b)
    };
    body.velocity = body.velocity + gravity * dt;

    let he = body.shape.half_extents();
    // Three axis-separated sweeps: X then Y then Z. The motion budget
    // for each axis is `velocity * dt`; on impact we clip the position
    // to the contact point, then apply restitution / friction.
    for axis in 0..3 {
        let v = component(body.velocity, axis);
        let motion = v * dt;
        if motion == 0.0 { continue; }
        let (t, normal_sign) = sweep_axis_world(world, body.position, he, axis, motion);
        let advanced = motion * t;
        body.position = add_axis(body.position, axis, advanced);
        if t < 1.0 {
            // Push the body just outside contact, then resolve.
            let recoil = -advanced.signum() * SKIN;
            body.position = add_axis(body.position, axis, recoil);
            // Reflect velocity along contact axis with restitution.
            let v_along = component(body.velocity, axis);
            let v_new = -v_along * body.restitution;
            let v_new = if v_new.abs() < REST_VELOCITY_EPS { 0.0 } else { v_new };
            body.velocity = set_axis(body.velocity, axis, v_new);

            // Friction on the tangential components.
            apply_friction(&mut body.velocity, axis, body.friction);

            let impulse = (v_along - v_new).abs() * body.mass.max(0.0);
            let normal = axis_normal(axis, normal_sign);
            let point = body.position + axis_vec(axis, advanced.signum() * he_component(he, axis));
            world.bodies.push_event(CollisionEvent {
                a: id.0,
                b: CollisionEvent::WORLD,
                point,
                normal,
                impulse,
            });
        }
    }

    world.bodies.slots[id.0 as usize] = Some(body);
}

/// AABB-vs-voxel-grid swept collision along a single axis. `motion`
/// is the desired displacement along the axis (signed). Returns
/// `(t, normal_sign)` where `t ∈ [0, 1]` is the fraction of motion
/// completed before contact (`t = 1` means clear path) and
/// `normal_sign` is `+1` if the body hit a face whose outward normal
/// points in the +axis direction, `-1` otherwise. When `t = 1` the
/// normal value is meaningless.
fn sweep_axis_world(
    world: &WorldState,
    center: Vec3,
    he: Vec3,
    axis: usize,
    motion: f32,
) -> (f32, f32) {
    if motion == 0.0 {
        return (1.0, 0.0);
    }
    let pos = component(center, axis);
    let half = he_component(he, axis);
    let other_axes = match axis { 0 => (1, 2), 1 => (0, 2), _ => (0, 1) };

    // For each "other axis" we determine the voxel-cell range the
    // body's AABB covers right now. We assume tangential motion is
    // negligible for this axis-separated step.
    let (a, b) = other_axes;
    let pa = component(center, a);
    let pb = component(center, b);
    let ha = he_component(he, a);
    let hb = he_component(he, b);

    let world_max = WORLD_SIDE as f32;
    if pa + ha < 0.0 || pa - ha >= world_max || pb + hb < 0.0 || pb - hb >= world_max {
        return (1.0, 0.0);
    }
    // Half-open `[lo, hi]` voxel-cell ranges along the other axes.
    let lo_a = (pa - ha).floor().max(0.0) as i32;
    let hi_a = ((pa + ha - SKIN).floor() as i32).max(lo_a);
    let lo_b = (pb - hb).floor().max(0.0) as i32;
    let hi_b = ((pb + hb - SKIN).floor() as i32).max(lo_b);
    let max_cell = (WORLD_SIDE as i32) - 1;
    let lo_a = lo_a.min(max_cell);
    let hi_a = hi_a.min(max_cell);
    let lo_b = lo_b.min(max_cell);
    let hi_b = hi_b.min(max_cell);

    if motion > 0.0 {
        // Leading face is the +axis face at pos + half. After motion it
        // sweeps to pos + half + motion. The voxel cells whose faces
        // are crossed sit at integer x values from
        // `ceil(pos + half)` up to `floor(pos + half + motion)`.
        let lead_start = pos + half;
        let lead_end = lead_start + motion;
        // First integer cell index whose left face lies strictly past
        // the current leading edge.
        let first = (lead_start + SKIN).floor() as i32 + 1;
        let last = lead_end.floor() as i32;
        if last < first || last < 0 {
            return (1.0, 0.0);
        }
        let first = first.max(0);
        let last = last.min(max_cell);
        for cell in first..=last {
            if column_solid(world, axis, cell, a, lo_a, hi_a, b, lo_b, hi_b) {
                let dist = cell as f32 - lead_start;
                let t = (dist / motion).clamp(0.0, 1.0);
                return (t, -1.0);
            }
        }
        (1.0, 0.0)
    } else {
        // Leading face is the -axis face at pos - half, moving in -axis.
        let lead_start = pos - half;
        let lead_end = lead_start + motion;
        // First integer face strictly past the current leading edge in -axis direction:
        // largest integer strictly less than lead_start.
        let first = (lead_start - SKIN).ceil() as i32 - 1;
        let last = lead_end.floor() as i32;
        if last > first || first < 0 {
            return (1.0, 0.0);
        }
        let first = first.min(max_cell);
        let last = last.max(0);
        for cell in (last..=first).rev() {
            if column_solid(world, axis, cell, a, lo_a, hi_a, b, lo_b, hi_b) {
                // Distance is (cell + 1) - lead_start; motion is negative.
                let dist = (cell as f32 + 1.0) - lead_start;
                let t = (dist / motion).clamp(0.0, 1.0);
                return (t, 1.0);
            }
        }
        (1.0, 0.0)
    }
}

/// True iff any non-air voxel exists in the cell column at axis
/// `axis = cell` for the cross-section `(a in lo_a..=hi_a, b in lo_b..=hi_b)`.
fn column_solid(
    world: &WorldState,
    axis: usize,
    cell: i32,
    a_axis: usize,
    lo_a: i32,
    hi_a: i32,
    b_axis: usize,
    lo_b: i32,
    hi_b: i32,
) -> bool {
    for a in lo_a..=hi_a {
        for b in lo_b..=hi_b {
            let (x, y, z) = match (axis, a_axis, b_axis) {
                (0, 1, 2) => (cell, a, b),
                (0, 2, 1) => (cell, b, a),
                (1, 0, 2) => (a, cell, b),
                (1, 2, 0) => (b, cell, a),
                (2, 0, 1) => (a, b, cell),
                (2, 1, 0) => (b, a, cell),
                _ => unreachable!(),
            };
            if x < 0 || y < 0 || z < 0 { continue; }
            if world.read_material(x as u32, y as u32, z as u32) != 0 {
                return true;
            }
        }
    }
    false
}

// ============================================================================
// Pair resolution
// ============================================================================

fn resolve_pairs(world: &mut WorldState) {
    // Collect (slot, body) snapshots once so we can iterate pairs
    // without re-borrowing.
    let mut snapshot: Vec<(u32, Body)> = world
        .bodies
        .iter_with_index()
        .map(|(i, b)| (i, *b))
        .collect();

    let n = snapshot.len();
    let mut events_pending: Vec<CollisionEvent> = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            let (ai, a) = snapshot[i];
            let (bi, b) = snapshot[j];
            if !layers_collide(a.layer, a.mask, b.layer, b.mask) {
                continue;
            }
            // Two statics ignore each other.
            if a.kind != BodyKind::Dynamic && b.kind != BodyKind::Dynamic {
                continue;
            }
            let Some(contact) = contact_between(&a, &b) else { continue };
            // Sensors: emit event and continue without resolving.
            if a.sensor || b.sensor {
                events_pending.push(CollisionEvent {
                    a: ai, b: bi,
                    point: contact.point,
                    normal: contact.normal,
                    impulse: 0.0,
                });
                continue;
            }
            let (left, right) = snapshot.split_at_mut(j);
            let (na, nb, impulse) = resolve_contact(&mut left[i].1, &mut right[0].1, contact);
            if na || nb {
                events_pending.push(CollisionEvent {
                    a: ai, b: bi,
                    point: contact.point,
                    normal: contact.normal,
                    impulse,
                });
            }
        }
    }

    // Write snapshot back.
    for (idx, body) in snapshot {
        world.bodies.slots[idx as usize] = Some(body);
    }
    for ev in events_pending {
        world.bodies.push_event(ev);
    }
}

#[derive(Copy, Clone)]
struct Contact {
    /// Normal pointing from `a` toward `b`.
    normal: Vec3,
    /// Penetration depth along `normal`. > 0.
    depth: f32,
    /// Approximate contact point in world space.
    point: Vec3,
}

fn contact_between(a: &Body, b: &Body) -> Option<Contact> {
    match (a.shape, b.shape) {
        (Shape::Aabb { .. }, Shape::Aabb { .. }) => contact_aabb_aabb(a, b),
        (Shape::Sphere { .. }, Shape::Sphere { .. }) => contact_sphere_sphere(a, b),
        (Shape::Aabb { .. }, Shape::Sphere { .. }) => contact_aabb_sphere(a, b, false),
        (Shape::Sphere { .. }, Shape::Aabb { .. }) => contact_aabb_sphere(b, a, true),
    }
}

fn contact_aabb_aabb(a: &Body, b: &Body) -> Option<Contact> {
    let (amin, amax) = a.aabb();
    let (bmin, bmax) = b.aabb();
    // Overlap on each axis.
    let ox = (amax.x.min(bmax.x) - amin.x.max(bmin.x)).max(0.0);
    let oy = (amax.y.min(bmax.y) - amin.y.max(bmin.y)).max(0.0);
    let oz = (amax.z.min(bmax.z) - amin.z.max(bmin.z)).max(0.0);
    if ox <= 0.0 || oy <= 0.0 || oz <= 0.0 {
        return None;
    }
    // Pick the minimum-translation axis.
    let (axis, depth) = if ox <= oy && ox <= oz {
        (0usize, ox)
    } else if oy <= oz {
        (1usize, oy)
    } else {
        (2usize, oz)
    };
    let dir = if component(b.position, axis) >= component(a.position, axis) { 1.0 } else { -1.0 };
    let normal = axis_normal(axis, dir);
    // Contact point ~ midpoint of overlap region center.
    let point = Vec3::new(
        (amin.x.max(bmin.x) + amax.x.min(bmax.x)) * 0.5,
        (amin.y.max(bmin.y) + amax.y.min(bmax.y)) * 0.5,
        (amin.z.max(bmin.z) + amax.z.min(bmax.z)) * 0.5,
    );
    Some(Contact { normal, depth, point })
}

fn contact_sphere_sphere(a: &Body, b: &Body) -> Option<Contact> {
    let ra = match a.shape { Shape::Sphere { radius } => radius, _ => return None };
    let rb = match b.shape { Shape::Sphere { radius } => radius, _ => return None };
    let d = b.position - a.position;
    let dist2 = d.dot(d);
    let r = ra + rb;
    if dist2 >= r * r { return None; }
    let dist = f32::sqrt(dist2.max(0.0));
    let (normal, depth) = if dist > 1e-6 {
        (d * (1.0 / dist), r - dist)
    } else {
        // Concentric — break symmetry along +X.
        (Vec3::X, r)
    };
    let point = a.position + normal * ra;
    Some(Contact { normal, depth, point })
}

fn contact_aabb_sphere(aabb: &Body, sphere: &Body, swapped: bool) -> Option<Contact> {
    let (amin, amax) = aabb.aabb();
    let r = match sphere.shape { Shape::Sphere { radius } => radius, _ => return None };
    let cx = sphere.position.x.clamp(amin.x, amax.x);
    let cy = sphere.position.y.clamp(amin.y, amax.y);
    let cz = sphere.position.z.clamp(amin.z, amax.z);
    let closest = Vec3::new(cx, cy, cz);
    let d = sphere.position - closest;
    let dist2 = d.dot(d);
    if dist2 >= r * r { return None; }
    let dist = f32::sqrt(dist2.max(0.0));
    let (normal_aabb_to_sphere, depth) = if dist > 1e-6 {
        (d * (1.0 / dist), r - dist)
    } else {
        (Vec3::Y, r)
    };
    // Normal must point from `a → b` in caller's argument order.
    let normal = if swapped { -normal_aabb_to_sphere } else { normal_aabb_to_sphere };
    Some(Contact { normal, depth, point: closest })
}

/// Resolve a body-pair contact. Pushes them apart along the contact
/// normal and applies a 1-D collision impulse with restitution +
/// friction tangentially. Returns `(a_moved, b_moved, impulse_mag)`.
fn resolve_contact(a: &mut Body, b: &mut Body, contact: Contact) -> (bool, bool, f32) {
    let (push_a, push_b) = match (a.kind, b.kind) {
        // Static/Kinematic ↔ Dynamic: only the Dynamic body moves.
        (BodyKind::Dynamic, BodyKind::Dynamic) => (0.5, 0.5),
        (BodyKind::Dynamic, _) => (1.0, 0.0),
        (_, BodyKind::Dynamic) => (0.0, 1.0),
        _ => return (false, false, 0.0), // two non-dynamic shouldn't reach here
    };
    let push = contact.normal * contact.depth;
    if push_a > 0.0 { a.position = a.position - push * push_a; }
    if push_b > 0.0 { b.position = b.position + push * push_b; }

    // Impulse along contact normal: relative velocity projected on normal.
    let rel = b.velocity - a.velocity;
    let v_along = rel.dot(contact.normal);
    if v_along >= 0.0 {
        // Already separating; no impulse needed.
        return (push_a > 0.0, push_b > 0.0, 0.0);
    }
    let e = (a.restitution.min(b.restitution)).clamp(0.0, 1.0);
    let inv_ma = inv_mass(a);
    let inv_mb = inv_mass(b);
    let inv_sum = inv_ma + inv_mb;
    if inv_sum <= 0.0 {
        return (push_a > 0.0, push_b > 0.0, 0.0);
    }
    let j = -(1.0 + e) * v_along / inv_sum;
    let jvec = contact.normal * j;
    if matches!(a.kind, BodyKind::Dynamic) {
        a.velocity = a.velocity - jvec * inv_ma;
    }
    if matches!(b.kind, BodyKind::Dynamic) {
        b.velocity = b.velocity + jvec * inv_mb;
    }

    // Coulomb friction along the tangent of relative velocity.
    let mu = (a.friction * b.friction).max(0.0).sqrt();
    if mu > 0.0 {
        let rel_after = b.velocity - a.velocity;
        let v_norm = contact.normal * rel_after.dot(contact.normal);
        let tangent = rel_after - v_norm;
        let tlen2 = tangent.dot(tangent);
        if tlen2 > 1e-8 {
            let tlen = f32::sqrt(tlen2);
            let tangent_unit = tangent * (1.0 / tlen);
            let jt = -tangent_unit.dot(rel_after) / inv_sum;
            let jt = jt.clamp(-mu * j.abs(), mu * j.abs());
            let jt_vec = tangent_unit * jt;
            if matches!(a.kind, BodyKind::Dynamic) {
                a.velocity = a.velocity - jt_vec * inv_ma;
            }
            if matches!(b.kind, BodyKind::Dynamic) {
                b.velocity = b.velocity + jt_vec * inv_mb;
            }
        }
    }

    (push_a > 0.0, push_b > 0.0, j.abs())
}

fn inv_mass(b: &Body) -> f32 {
    match b.kind {
        BodyKind::Dynamic if b.mass > 0.0 => 1.0 / b.mass,
        _ => 0.0,
    }
}

fn layers_collide(la: u8, ma: u8, lb: u8, mb: u8) -> bool {
    (ma & (1 << lb)) != 0 && (mb & (1 << la)) != 0
}

// ============================================================================
// Actor sync
// ============================================================================

/// After integration, copy each body's position into its attached actor
/// so the renderer's actor draw follows the simulated body.
fn sync_actors(world: &mut WorldState) {
    let n = world.bodies.capacity_slots();
    for i in 0..n {
        let Some(body) = world.bodies.slots[i].as_ref() else { continue };
        let Some(actor_id) = body.actor else { continue };
        let he = body.shape.half_extents();
        let actor_pos = body.position - he;
        if let Some(actor) = world.actors.get_mut(actor_id) {
            actor.position = actor_pos;
        }
    }
}

/// Find any body attached to a given actor — used when the cart wants
/// to despawn a body via the actor it controls. Linear scan, O(bodies).
pub fn body_for_actor(table: &BodyTable, actor: ActorId) -> Option<BodyId> {
    table
        .slots
        .iter()
        .enumerate()
        .find_map(|(i, slot)| {
            slot.as_ref()
                .and_then(|b| b.actor)
                .filter(|a| *a == actor)
                .map(|_| BodyId(i as u32))
        })
}

// ============================================================================
// Encoders
// ============================================================================

/// Encode `BodyState` to its 60-byte wire form. Used by sandbox.rs
/// when writing the result back to cart memory via `body_get`.
pub fn encode_body_state(st: &BodyState) -> [u8; 60] {
    let mut out = [0u8; 60];
    out[0] = st.kind;
    out[1] = st.shape_tag;
    out[2] = st.layer;
    out[3] = st.mask;
    out[4] = st.sensor;
    // bytes 5..8 are pad
    out[8..12].copy_from_slice(&st.shape[0].to_le_bytes());
    out[12..16].copy_from_slice(&st.shape[1].to_le_bytes());
    out[16..20].copy_from_slice(&st.shape[2].to_le_bytes());
    out[20..24].copy_from_slice(&st.position.x.to_le_bytes());
    out[24..28].copy_from_slice(&st.position.y.to_le_bytes());
    out[28..32].copy_from_slice(&st.position.z.to_le_bytes());
    out[32..36].copy_from_slice(&st.velocity.x.to_le_bytes());
    out[36..40].copy_from_slice(&st.velocity.y.to_le_bytes());
    out[40..44].copy_from_slice(&st.velocity.z.to_le_bytes());
    out[44..48].copy_from_slice(&st.mass.to_le_bytes());
    out[48..52].copy_from_slice(&st.restitution.to_le_bytes());
    out[52..56].copy_from_slice(&st.friction.to_le_bytes());
    out[56..60].copy_from_slice(&st.actor.to_le_bytes());
    out
}

pub fn encode_collision_event(ev: &CollisionEvent) -> [u8; 36] {
    let mut out = [0u8; 36];
    out[0..4].copy_from_slice(&ev.a.to_le_bytes());
    out[4..8].copy_from_slice(&ev.b.to_le_bytes());
    out[8..12].copy_from_slice(&ev.point.x.to_le_bytes());
    out[12..16].copy_from_slice(&ev.point.y.to_le_bytes());
    out[16..20].copy_from_slice(&ev.point.z.to_le_bytes());
    out[20..24].copy_from_slice(&ev.normal.x.to_le_bytes());
    out[24..28].copy_from_slice(&ev.normal.y.to_le_bytes());
    out[28..32].copy_from_slice(&ev.normal.z.to_le_bytes());
    out[32..36].copy_from_slice(&ev.impulse.to_le_bytes());
    out
}

// ============================================================================
// Vec3 / axis helpers
// ============================================================================

fn component(v: Vec3, axis: usize) -> f32 {
    match axis { 0 => v.x, 1 => v.y, _ => v.z }
}
fn add_axis(v: Vec3, axis: usize, d: f32) -> Vec3 {
    match axis {
        0 => Vec3::new(v.x + d, v.y, v.z),
        1 => Vec3::new(v.x, v.y + d, v.z),
        _ => Vec3::new(v.x, v.y, v.z + d),
    }
}
fn set_axis(v: Vec3, axis: usize, val: f32) -> Vec3 {
    match axis {
        0 => Vec3::new(val, v.y, v.z),
        1 => Vec3::new(v.x, val, v.z),
        _ => Vec3::new(v.x, v.y, val),
    }
}
fn axis_normal(axis: usize, sign: f32) -> Vec3 {
    match axis {
        0 => Vec3::new(sign, 0.0, 0.0),
        1 => Vec3::new(0.0, sign, 0.0),
        _ => Vec3::new(0.0, 0.0, sign),
    }
}
fn axis_vec(axis: usize, len: f32) -> Vec3 {
    match axis {
        0 => Vec3::new(len, 0.0, 0.0),
        1 => Vec3::new(0.0, len, 0.0),
        _ => Vec3::new(0.0, 0.0, len),
    }
}
fn he_component(he: Vec3, axis: usize) -> f32 { component(he, axis) }

fn apply_friction(v: &mut Vec3, contact_axis: usize, mu: f32) {
    if mu <= 0.0 { return; }
    let drop = mu;
    match contact_axis {
        0 => {
            v.y -= v.y * drop;
            v.z -= v.z * drop;
        }
        1 => {
            v.x -= v.x * drop;
            v.z -= v.z * drop;
        }
        _ => {
            v.x -= v.x * drop;
            v.y -= v.y * drop;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_world() -> WorldState {
        let mut w = WorldState::new();
        w.flush();
        w.macro_grid.rebuild(&w.actors);
        w
    }

    fn dynamic_aabb(pos: Vec3, extents: Vec3) -> Body {
        Body {
            kind: BodyKind::Dynamic,
            shape: Shape::Aabb { extents },
            position: pos,
            velocity: Vec3::ZERO,
            mass: 1.0,
            restitution: 0.0,
            friction: 0.0,
            layer: 0,
            mask: 0xFF,
            sensor: false,
            actor: None,
        }
    }

    #[test]
    fn gravity_pulls_body_down() {
        let mut w = empty_world();
        w.bodies.gravity = Vec3::new(0.0, -10.0, 0.0);
        let id = w.bodies.spawn(dynamic_aabb(Vec3::new(50.0, 50.0, 50.0), Vec3::splat(1.0)))
            .expect("spawn");
        step(&mut w, 1.0 / 60.0);
        let b = w.bodies.get(id).unwrap();
        assert!(b.velocity.y < 0.0, "v.y should be negative, got {:?}", b.velocity);
        assert!(b.position.y < 50.0, "should have fallen, got {:?}", b.position);
    }

    #[test]
    fn body_settles_on_solid_floor() {
        let mut w = WorldState::new();
        // 5x5 platform of stone at y=10.
        for x in 8..14u32 {
            for z in 8..14u32 {
                w.set_voxel(x, 10, z, 4);
            }
        }
        w.flush();
        w.bodies.gravity = Vec3::new(0.0, -20.0, 0.0);
        let id = w.bodies.spawn(dynamic_aabb(
            Vec3::new(11.0, 14.0, 11.0),
            Vec3::splat(1.0),
        )).expect("spawn");
        // 60 substeps = 1 second simulated. Should land.
        for _ in 0..60 { step(&mut w, 1.0 / 60.0); }
        let b = w.bodies.get(id).unwrap();
        // Floor top is at y=11 (voxel cell 10 occupies y in [10, 11)). Hmm,
        // wait: voxel cell y=10 occupies y in [10, 11], with top face at
        // y=11. Body half-extent y=0.5, so resting center should be ~11.5.
        assert!(b.position.y > 11.4 && b.position.y < 11.6,
            "body should rest just above the platform, got {:?}", b.position);
        assert!(b.velocity.y.abs() < 0.1, "should be stopped, got vy={}", b.velocity.y);
    }

    #[test]
    fn body_vs_body_aabb_separates_and_emits_event() {
        let mut w = empty_world();
        let a = w.bodies.spawn(Body {
            position: Vec3::new(50.0, 50.0, 50.0),
            velocity: Vec3::X,
            ..dynamic_aabb(Vec3::ZERO, Vec3::splat(1.0))
        }).unwrap();
        let b = w.bodies.spawn(Body {
            position: Vec3::new(50.5, 50.0, 50.0), // overlapping by 0.5
            velocity: -Vec3::X,
            ..dynamic_aabb(Vec3::ZERO, Vec3::splat(1.0))
        }).unwrap();
        step(&mut w, 1.0 / 60.0);
        let pa = w.bodies.get(a).unwrap().position;
        let pb = w.bodies.get(b).unwrap().position;
        assert!(pb.x - pa.x >= 1.0 - 1e-3,
            "bodies should be separated, got dx={}", pb.x - pa.x);
        assert!(!w.bodies.events.is_empty(), "expected a collision event");
    }

    #[test]
    fn sphere_vs_sphere_separates() {
        let mut w = empty_world();
        let a = w.bodies.spawn(Body {
            shape: Shape::Sphere { radius: 0.5 },
            position: Vec3::new(50.0, 50.0, 50.0),
            ..dynamic_aabb(Vec3::ZERO, Vec3::ZERO)
        }).unwrap();
        let b = w.bodies.spawn(Body {
            shape: Shape::Sphere { radius: 0.5 },
            position: Vec3::new(50.6, 50.0, 50.0),
            ..dynamic_aabb(Vec3::ZERO, Vec3::ZERO)
        }).unwrap();
        step(&mut w, 1.0 / 60.0);
        let pa = w.bodies.get(a).unwrap().position;
        let pb = w.bodies.get(b).unwrap().position;
        let d = pb - pa;
        let dist = f32::sqrt(d.dot(d));
        assert!(dist >= 1.0 - 1e-3, "spheres should be separated, got dist={dist}");
    }

    #[test]
    fn layer_mask_excludes_pair() {
        let mut w = empty_world();
        let a = w.bodies.spawn(Body {
            layer: 0, mask: 0b0010,
            position: Vec3::new(50.0, 50.0, 50.0),
            ..dynamic_aabb(Vec3::ZERO, Vec3::splat(1.0))
        }).unwrap();
        let b = w.bodies.spawn(Body {
            layer: 0, mask: 0b0010, // b in layer 0 but masks out layer 0 too
            position: Vec3::new(50.5, 50.0, 50.0),
            ..dynamic_aabb(Vec3::ZERO, Vec3::splat(1.0))
        }).unwrap();
        step(&mut w, 1.0 / 60.0);
        let _ = (a, b);
        // No events because layers exclude.
        assert!(w.bodies.events.is_empty(),
            "no event expected when layers exclude, got {:?}", w.bodies.events);
    }

    #[test]
    fn sensor_emits_event_without_resolving() {
        let mut w = empty_world();
        let a = w.bodies.spawn(Body {
            position: Vec3::new(50.0, 50.0, 50.0),
            sensor: true,
            ..dynamic_aabb(Vec3::ZERO, Vec3::splat(1.0))
        }).unwrap();
        let b = w.bodies.spawn(Body {
            position: Vec3::new(50.5, 50.0, 50.0),
            ..dynamic_aabb(Vec3::ZERO, Vec3::splat(1.0))
        }).unwrap();
        step(&mut w, 1.0 / 60.0);
        // Bodies should still overlap (sensor didn't push).
        let pa = w.bodies.get(a).unwrap().position;
        let pb = w.bodies.get(b).unwrap().position;
        assert!((pb.x - pa.x).abs() < 1.0 - 1e-3,
            "sensor shouldn't separate, got dx={}", pb.x - pa.x);
        assert!(!w.bodies.events.is_empty(), "sensor should emit event");
    }

    #[test]
    fn kinematic_pushes_dynamic() {
        let mut w = empty_world();
        let k = w.bodies.spawn(Body {
            kind: BodyKind::Kinematic,
            position: Vec3::new(50.0, 50.0, 50.0),
            ..dynamic_aabb(Vec3::ZERO, Vec3::splat(1.0))
        }).unwrap();
        let d = w.bodies.spawn(Body {
            position: Vec3::new(50.5, 50.0, 50.0),
            ..dynamic_aabb(Vec3::ZERO, Vec3::splat(1.0))
        }).unwrap();
        step(&mut w, 1.0 / 60.0);
        let pk = w.bodies.get(k).unwrap().position;
        let pd = w.bodies.get(d).unwrap().position;
        // Kinematic stays put, dynamic gets pushed +x.
        assert!((pk.x - 50.0).abs() < 1e-3, "kinematic moved: {}", pk.x);
        assert!(pd.x > 50.5, "dynamic should be pushed +x, got {}", pd.x);
    }

    #[test]
    fn event_queue_caps_and_drops_oldest() {
        let mut w = empty_world();
        let dummy = CollisionEvent {
            a: 0, b: CollisionEvent::WORLD,
            point: Vec3::ZERO, normal: Vec3::Y, impulse: 0.0,
        };
        for i in 0..MAX_EVENTS + 10 {
            w.bodies.push_event(CollisionEvent { a: i as u32, ..dummy });
        }
        assert_eq!(w.bodies.events.len(), MAX_EVENTS);
        // First event should now be the 10th queued (0..9 dropped).
        assert_eq!(w.bodies.events[0].a, 10);
        assert_eq!(w.bodies.events_dropped, 10);
    }

    #[test]
    fn body_actor_sync_writes_position() {
        let mut w = empty_world();
        let actor = w.actors.spawn().unwrap();
        let id = w.bodies.spawn(Body {
            position: Vec3::new(50.0, 50.0, 50.0),
            velocity: Vec3::ZERO,
            actor: Some(actor),
            ..dynamic_aabb(Vec3::ZERO, Vec3::splat(2.0))
        }).unwrap();
        step(&mut w, 1.0 / 60.0);
        let actor_pos = w.actors.get(actor).unwrap().position;
        // half-extents are (1, 1, 1); actor.position is at body.position - he.
        assert!((actor_pos.x - 49.0).abs() < 1e-3, "actor x: {}", actor_pos.x);
        let _ = id;
    }

    #[test]
    fn body_cap_returns_none_at_limit() {
        let mut w = empty_world();
        let body = dynamic_aabb(Vec3::ZERO, Vec3::splat(1.0));
        for _ in 0..MAX_BODIES {
            assert!(w.bodies.spawn(body).is_some());
        }
        assert!(w.bodies.spawn(body).is_none());
    }

}
