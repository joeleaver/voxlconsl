//! §10.2 rigid-body demo: a stack of dynamic AABB crates + a pair of
//! sphere-shaped leaf balls that drop in when the player enters the
//! gameplay scene.
//!
//! The cart-side responsibility is just "spawn an actor, spawn a body,
//! point them at each other". The host integrator (gravity +
//! axis-separated voxel sweep for AABBs, discrete CCD for spheres,
//! pairwise body-vs-body contact) runs in `bodies::step` and writes
//! each body's world position into its attached actor on every step —
//! the renderer follows the simulation with no further cart code.

use voxlconsl_sdk::*;
use voxlconsl_sdk::bodies;

use crate::{M_CRATE, M_LEAF, WORLD};
use crate::player::PLAYER_POS;

const CRATE_STACK_LEN: usize = 3;
const CRATE_SIDE_VOX:  u8    = 3;
/// One side of the leaf-ball actor volume. The radius of the
/// simulated sphere is half this. We mask-paint only voxels inside
/// the sphere so the actor reads as a chunky ball rather than a cube;
/// the §10.2 CCD treats the body as a true sphere regardless.
const BALL_SIDE_VOX: u8    = 5;
const BALL_COUNT:    usize = 2;

static mut SPAWNED: bool = false;

/// Drop a short crate stack + two leaf balls a few voxels east of the
/// player. Subsequent calls are no-ops (one-shot demo).
pub(crate) fn spawn_demo_stack() {
    if unsafe { SPAWNED } { return; }
    unsafe { SPAWNED = true; }

    let p = unsafe { PLAYER_POS };
    // Spawn a few voxels east of the player so the dude doesn't get
    // buried by his own crates on the first frame.
    let base_x = (p.x + 4.0).clamp(2.0, (WORLD - 6) as f32);
    let base_z = (p.z + 0.5).clamp(2.0, (WORLD - 6) as f32);

    // Drop from well above terrain so gravity has room to act. With
    // g=-10 and ~10 voxels of fall, crates hit at ~14 v/s →
    // mostly absorbed by friction=0.5, restitution=0.15.
    let drop_height = p.y + 10.0;

    spawn_crates(base_x, base_z, drop_height);
    spawn_balls(p.x, p.z, drop_height);
}

fn spawn_crates(base_x: f32, base_z: f32, drop_height: f32) {
    let s = CRATE_SIDE_VOX as f32;
    for i in 0..CRATE_STACK_LEN {
        let Some(actor) = actor_spawn() else { break };
        // Fill the actor's volume with crate material. Default actor
        // size is DEFAULT_VOLUME_SIDE (16); we paint just the low
        // s×s×s corner. `bodies::step` writes actor.position to
        // (body.position - half_extents), so the painted cube lines
        // up with the simulated AABB.
        actor_fill_box(
            actor,
            U8Vec3::new(0, 0, 0),
            U8Vec3::new(CRATE_SIDE_VOX - 1, CRATE_SIDE_VOX - 1, CRATE_SIDE_VOX - 1),
            M_CRATE,
        );
        let Some(body) = bodies::body_spawn(
            Some(actor),
            BodyKind::Dynamic,
            Shape::Aabb { extents: Vec3::splat(s) },
            /*mass*/ 1.0,
        ) else { actor_despawn(actor); break };
        // Stack on top of each other; small lateral jitter so the
        // contact pair-resolver has a non-degenerate normal.
        let jitter = ((i as f32) - 1.0) * 0.05;
        let pos = Vec3::new(
            base_x + s * 0.5 + jitter,
            drop_height + (i as f32) * (s + 0.1),
            base_z + s * 0.5,
        );
        bodies::body_set_position(body, pos);
        // Low restitution so they don't bounce forever; moderate
        // friction so they don't slide across the dirt indefinitely.
        bodies::body_set_material(body, /*restitution*/0.15, /*friction*/0.5);
    }
}

fn spawn_balls(px: f32, pz: f32, drop_height: f32) {
    // Spheres demo the §10.2 sphere-vs-voxel CCD. Each actor paints
    // only the voxels inside the inscribed sphere (so the rendered
    // shape reads as a ball, not a cube), while the host integrator
    // uses the spherical body with proper CCD.
    let radius = (BALL_SIDE_VOX as f32) * 0.5;
    let ball_base_x = (px - 5.0).clamp(2.0, (WORLD - 6) as f32);
    let ball_base_z = (pz + 1.0).clamp(2.0, (WORLD - 6) as f32);

    for i in 0..BALL_COUNT {
        let Some(actor) = actor_spawn() else { break };
        // Sphere-mask paint. Any voxel whose center-to-center distance
        // to the cube's geometric center is ≤ `radius - 0.05` gets
        // painted. The small shrinkage keeps the visible silhouette
        // inside the body's bounding sphere so the renderer never has
        // voxels poking out past the simulated contact surface.
        let r_paint = radius - 0.05;
        let r2 = r_paint * r_paint;
        let center = (BALL_SIDE_VOX as f32 - 1.0) * 0.5;
        for vz in 0..BALL_SIDE_VOX {
            for vy in 0..BALL_SIDE_VOX {
                for vx in 0..BALL_SIDE_VOX {
                    let dx = vx as f32 - center;
                    let dy = vy as f32 - center;
                    let dz = vz as f32 - center;
                    if dx*dx + dy*dy + dz*dz <= r2 {
                        actor_set_voxel(actor, U8Vec3::new(vx, vy, vz), M_LEAF);
                    }
                }
            }
        }
        let Some(body) = bodies::body_spawn(
            Some(actor),
            BodyKind::Dynamic,
            Shape::Sphere { radius },
            /*mass*/ 0.5,
        ) else { actor_despawn(actor); break };
        let pos = Vec3::new(
            ball_base_x + radius,
            drop_height + (i as f32) * (radius * 2.0 + 0.2),
            ball_base_z + radius,
        );
        bodies::body_set_position(body, pos);
        // Springier than crates — balls bounce a bit on impact and
        // roll a touch before friction takes them.
        bodies::body_set_material(body, /*restitution*/0.45, /*friction*/0.25);
    }
}
