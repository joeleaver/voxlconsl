//! Pooled chomp-burst sparkles.
//!
//! Every dot pickup spawns a small arc of 2×2×2 yellow cube actors
//! that arc outward, fall under cart-local gravity, and despawn back
//! into the pool on TTL or floor contact. No collision against the
//! world — particles just fall through.
//!
//! The pool is pre-spawned at boot (one `actor_spawn` per slot, all
//! hidden) so per-pickup spawning is just a "flip visible + assign
//! velocity" operation — no actor allocation during gameplay.

use voxlconsl_sdk::*;

use crate::M_PARTICLE;
use crate::rng;

const CAP:               usize    = 24;
pub(crate) const PER_BURST: usize  = 5;
const TTL_MS:            u32      = 700;
const GRAVITY:           f32      = 0.040;
const W:                 u32      = 2;
const VOL:               usize    = (W * W * W) as usize;
const P_PARTICLE:        PrefabId = PrefabId(3);

#[derive(Copy, Clone)]
struct Particle {
    actor:  Option<ActorId>,
    pos:    Vec3,
    vel:    Vec3,
    ttl_ms: u32,
    active: bool,
}

static mut PARTICLES: [Particle; CAP] = [Particle {
    actor:  None,
    pos:    Vec3 { x: 0.0, y: 0.0, z: 0.0 },
    vel:    Vec3 { x: 0.0, y: 0.0, z: 0.0 },
    ttl_ms: 0,
    active: false,
}; CAP];

static mut DENSE: [u8; VOL] = [M_PARTICLE; VOL];

/// Pre-spawn the pool of hidden particle actors. Called once from
/// the cart's `init`.
pub(crate) fn init() {
    unsafe {
        prefab_define(P_PARTICLE, &*(&raw const DENSE), U8Vec3::new(W as u8, W as u8, W as u8));
        let particles = &mut *(&raw mut PARTICLES);
        for p in particles.iter_mut() {
            let id = actor_spawn_from(P_PARTICLE, Orientation::Up)
                .expect("failed to spawn particle");
            actor_set_visible(id, false);
            p.actor = Some(id);
            p.active = false;
        }
    }
}

/// Spawn up to `n` particles arcing outward from `centre`. Picks
/// inactive pool slots first; if the pool is exhausted the surplus is
/// dropped silently rather than reusing a still-airborne slot.
pub(crate) fn spawn_burst(centre: Vec3, n: usize) {
    let particles = unsafe { &mut *(&raw mut PARTICLES) };
    let mut spawned = 0;
    for p in particles.iter_mut() {
        if spawned >= n { break; }
        if p.active { continue; }
        let actor = match p.actor { Some(a) => a, None => continue };

        // Outward velocity: random in the xz plane, biased upward in
        // y so particles initially launch into the air before gravity
        // arcs them back down.
        let vx = rng::signed() * 0.55;
        let vy = 0.65 + rng::unit() * 0.45;
        let vz = rng::signed() * 0.55;

        p.pos = Vec3::new(
            centre.x - W as f32 * 0.5,
            // Lift the start so particles don't clip into the floor.
            centre.y + 1.5,
            centre.z - W as f32 * 0.5,
        );
        p.vel = Vec3::new(vx, vy, vz);
        p.ttl_ms = TTL_MS;
        p.active = true;
        actor_set_visible(actor, true);
        actor_set_position(actor, p.pos);
        spawned += 1;
    }
}

/// Integrate every active particle by one frame: Euler step on
/// gravity, despawn on TTL expiry or floor contact.
pub(crate) fn tick(dt_ms: u32) {
    let particles = unsafe { &mut *(&raw mut PARTICLES) };
    for p in particles.iter_mut() {
        if !p.active { continue; }
        let actor = match p.actor { Some(a) => a, None => continue };

        // Single Euler step per frame — particles never collide, so
        // accuracy doesn't matter; only visual smoothness.
        p.vel.y -= GRAVITY;
        p.pos.x += p.vel.x;
        p.pos.y += p.vel.y;
        p.pos.z += p.vel.z;

        let ttl_done = p.ttl_ms <= dt_ms;
        if ttl_done || p.pos.y < 0.0 {
            actor_set_visible(actor, false);
            p.active = false;
            continue;
        }
        p.ttl_ms -= dt_ms;
        actor_set_position(actor, p.pos);
    }
}

/// Hide every airborne particle — used by `restart_game` so a
/// new round doesn't inherit lingering sparkles from the prior one.
pub(crate) fn clear_all() {
    let particles = unsafe { &mut *(&raw mut PARTICLES) };
    for p in particles.iter_mut() {
        if p.active {
            if let Some(a) = p.actor { actor_set_visible(a, false); }
            p.active = false;
        }
    }
}
