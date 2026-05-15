//! 512×512 terrain generation: multi-octave value noise → heightmap →
//! voxel columns (stone / dirt band / grass cap), then ~500
//! deterministically-placed trees scattered across grass cells.
//!
//! The whole thing runs once during `init`. fill_box collapses each
//! voxel column to one host call — a per-voxel `set_voxel` would be a
//! half-million extra round-trips on this 512² map.

use voxlconsl_sdk::*;

use crate::{M_DIRT, M_GRASS, M_LEAF, M_STONE, M_WOOD, WORLD};

// ── Value noise ──────────────────────────────────────────────────

/// Hash 2D integer coords into a deterministic float in `[0, 1)`.
fn hash2(ix: i32, iz: i32) -> f32 {
    let mut h = (ix as u32)
        .wrapping_mul(0x1657_8E37)
        .wrapping_add((iz as u32).wrapping_mul(0xB7E1_5163));
    h ^= h >> 13;
    h = h.wrapping_mul(0x4BC0_3937);
    h ^= h >> 16;
    (h as f32) * (1.0 / 4_294_967_296.0)
}

fn smoothstep(t: f32) -> f32 { t * t * (3.0 - 2.0 * t) }

fn value_noise_2d(x: f32, z: f32) -> f32 {
    // Manual floor for non-negative inputs (we never sample negative
    // coords; std::f32::floor isn't available in no_std without libm).
    let ix = x as i32;
    let iz = z as i32;
    let fx = x - ix as f32;
    let fz = z - iz as f32;

    let v00 = hash2(ix,     iz);
    let v10 = hash2(ix + 1, iz);
    let v01 = hash2(ix,     iz + 1);
    let v11 = hash2(ix + 1, iz + 1);

    let sx = smoothstep(fx);
    let sz = smoothstep(fz);

    let a = v00 + (v10 - v00) * sx;
    let b = v01 + (v11 - v01) * sx;
    a + (b - a) * sz
}

/// Sample the heightmap at world `(x, z)`. Returns integer voxel
/// height in `[4, 28]`.
pub(crate) fn terrain_height(x: u32, z: u32) -> u32 {
    let mut h = 0.0_f32;
    let mut amp = 1.0_f32;
    let mut freq = 1.0_f32 / 64.0;
    let mut total = 0.0_f32;
    for _ in 0..4 {
        h += value_noise_2d(x as f32 * freq, z as f32 * freq) * amp;
        total += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    h /= total;
    (4.0 + h * 24.0) as u32
}

// ── Painting ──────────────────────────────────────────────────────

/// Walk every (x, z) column on the 512×512 grid and paint stone /
/// dirt band / grass-cap voxels according to the heightmap.
pub(crate) fn paint_ground() {
    for z in 0..WORLD {
        for x in 0..WORLD {
            let h = terrain_height(x, z);
            // Stone fill.
            if h > 4 {
                fill_box(UVec3::new(x, 0, z), UVec3::new(x, h - 4, z), M_STONE);
            }
            // Dirt band right under the surface.
            if h >= 2 {
                let dirt_lo = if h > 3 { h - 3 } else { 0 };
                fill_box(UVec3::new(x, dirt_lo, z), UVec3::new(x, h - 2, z), M_DIRT);
            }
            // Grass surface.
            if h > 0 {
                set_voxel(UVec3::new(x, h - 1, z), M_GRASS);
            }
        }
    }
}

/// Scatter ~500 trees with an LCG-driven placement so the forest is
/// deterministic across runs. Trees are only planted on tiles where
/// the terrain is high enough (h ≥ 8) to avoid populating ditches and
/// underwater spots.
pub(crate) fn scatter_trees() {
    let mut prng = 0xDEAD_BEEFu32;
    let mut planted = 0u32;
    while planted < 500 {
        prng = prng.wrapping_mul(0x9E37_79B9).wrapping_add(0x1234_5678);
        // Canopy spans cx±3, cz±3 → keep a 4-voxel border from edges.
        let tx = ((prng >> 8) % (WORLD - 10)) + 5;
        prng = prng.wrapping_mul(0x9E37_79B9).wrapping_add(0x1234_5678);
        let tz = ((prng >> 8) % (WORLD - 10)) + 5;
        let h = terrain_height(tx, tz);
        if h >= 8 {
            plant_tree(tx, tz, h, prng);
            planted += 1;
        }
    }
}

/// Plant a tree at `(cx, cz)` with its base at world y=`base`.
/// `variant` (any u32) drives a small height variation so the forest
/// doesn't look like a stamp pattern. Total tree height ≈ 8–10
/// voxels (taller than the 7-tall dude); 4-layer canopy shrinking
/// from a 7×7 mid-ring to a 3×3 cap.
fn plant_tree(cx: u32, cz: u32, base: u32, variant: u32) {
    let trunk_h = 4 + (variant % 3);  // 4, 5, or 6
    let trunk_top = base + trunk_h;
    fill_box(
        UVec3::new(cx, base, cz),
        UVec3::new(cx, trunk_top - 1, cz),
        M_WOOD,
    );
    let l0 = trunk_top;
    let l1 = trunk_top + 1;
    let l2 = trunk_top + 2;
    let l3 = trunk_top + 3;
    // 5×5 base
    fill_box(UVec3::new(cx - 2, l0, cz - 2), UVec3::new(cx + 2, l0, cz + 2), M_LEAF);
    // 7×7 mid ring — the visually dominant layer
    fill_box(UVec3::new(cx - 3, l1, cz - 3), UVec3::new(cx + 3, l1, cz + 3), M_LEAF);
    // 5×5 upper
    fill_box(UVec3::new(cx - 2, l2, cz - 2), UVec3::new(cx + 2, l2, cz + 2), M_LEAF);
    // 3×3 cap
    fill_box(UVec3::new(cx - 1, l3, cz - 1), UVec3::new(cx + 1, l3, cz + 1), M_LEAF);
}
