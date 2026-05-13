//! World generation: heightmap + forest + lake + town + heli pad.
//!
//! All cart geometry sits inside a 192×192 footprint centered in the
//! 512³ scene — leaves ample headroom above for embers + chopper
//! flight altitude.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics;

use crate::rng::Rng;
use crate::{
    M_CABIN_ROOF, M_CABIN_WOOD, M_DIRT, M_FIRE, M_GRASS, M_HELI_PAD,
    M_PINE_LEAVES, M_PINE_WOOD, M_ROAD_DIRT, M_STONE, M_WATER,
};

// ── Footprint ─────────────────────────────────────────────────────

pub(crate) const FOOT_MIN: u32 = 32;
pub(crate) const FOOT_MAX: u32 = 224;          // exclusive upper bound
pub(crate) const FOOT_LEN: u32 = FOOT_MAX - FOOT_MIN;

// Anchors used by the rest of the cart.
//   The lake sits in the west, where the chopper refills.
//   The town clusters in the south-east, along a horizontal road.
//   The heli pad sits between them on the road.
//   The fire seed starts in the north-west — far from town, so the
//   player has time to react before flames reach the buildings.

pub(crate) const LAKE_CX: u32 = 60;
pub(crate) const LAKE_CZ: u32 = 110;
pub(crate) const LAKE_R:  u32 = 14;            // radius in voxels

pub(crate) const TOWN_MIN_X: u32 = 140;
pub(crate) const TOWN_MAX_X: u32 = 200;
pub(crate) const TOWN_Z:     u32 = 170;        // road runs east-west at this z

pub(crate) const HELI_PAD_X: u32 = 120;
pub(crate) const HELI_PAD_Z: u32 = 170;

pub(crate) const FIRE_SEED_X: u32 = 70;
pub(crate) const FIRE_SEED_Z: u32 = 70;

// Six cabins along the road, alternating north / south of it so the
// road threads through the town.
pub(crate) const CABIN_COUNT: usize = 6;
pub(crate) const CABINS: [(u32, u32); CABIN_COUNT] = [
    (148, 162), (158, 178), (170, 162),
    (180, 178), (192, 162), (200, 178),
];
const CABIN_SX: u32 = 7;
const CABIN_SZ: u32 = 6;
const CABIN_WALL_H: u32 = 4;
const CABIN_ROOF_H: u32 = 2;

// ── Value noise ───────────────────────────────────────────────────

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

/// Gentle 3-octave FBM. Returns voxel height in `[5, 16]`.
pub(crate) fn terrain_height(x: u32, z: u32) -> u32 {
    let mut h = 0.0_f32;
    let mut amp = 1.0_f32;
    let mut freq = 1.0_f32 / 48.0;
    let mut total = 0.0_f32;
    for _ in 0..3 {
        h += value_noise_2d(x as f32 * freq, z as f32 * freq) * amp;
        total += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    h /= total;
    (5.0 + h * 11.0) as u32
}

// ── Top-level paint ──────────────────────────────────────────────

pub(crate) fn paint_world() {
    paint_columns();
    paint_lake();
    paint_road_and_pad();
    paint_cabins();
    scatter_pines();
}

/// One voxel column per (x, z): stone fill, dirt band, grass cap.
fn paint_columns() {
    for z in FOOT_MIN..FOOT_MAX {
        for x in FOOT_MIN..FOOT_MAX {
            let h = terrain_height(x, z);
            if h > 4 {
                fill_box(UVec3::new(x, 0, z), UVec3::new(x, h - 4, z), M_STONE);
            }
            if h >= 2 {
                let dirt_lo = if h > 3 { h - 3 } else { 0 };
                fill_box(UVec3::new(x, dirt_lo, z), UVec3::new(x, h - 2, z), M_DIRT);
            }
            if h > 0 {
                set_voxel(UVec3::new(x, h - 1, z), M_GRASS);
            }
        }
    }
}

/// Carve a roughly-circular lake and fill it with water up to a level
/// matched to the lowest surrounding terrain. The cart's helicopter
/// dips here to refill.
fn paint_lake() {
    let cx = LAKE_CX;
    let cz = LAKE_CZ;
    let r = LAKE_R;
    let level = 6u32;

    for dz in -(r as i32)..=(r as i32) {
        for dx in -(r as i32)..=(r as i32) {
            if dx * dx + dz * dz > (r as i32) * (r as i32) { continue; }
            let x = (cx as i32 + dx) as u32;
            let z = (cz as i32 + dz) as u32;
            // Rebuild the column from scratch: stone floor, dirt
            // shoreline, water at `level`, then clear everything
            // above so a hill that happens to overlap the circle
            // doesn't leave a floating grass cap.
            fill_box(UVec3::new(x, 0, z), UVec3::new(x, level - 2, z), M_STONE);
            set_voxel(UVec3::new(x, level - 1, z), M_DIRT);
            set_voxel(UVec3::new(x, level, z), M_WATER);
            fill_box(UVec3::new(x, level + 1, z), UVec3::new(x, 24, z), 0);
        }
    }
}

/// Lay a wide road of M_ROAD_DIRT through the town strip + paint a
/// distinct heli pad square where the chopper idles.
fn paint_road_and_pad() {
    // Road: 3-wide strip at z = TOWN_Z running x = 100..210 along the
    // terrain surface.
    for x in 100u32..210 {
        for dz in 0u32..3 {
            let z = TOWN_Z + dz - 1;
            let h = terrain_height(x, z);
            if h == 0 { continue; }
            set_voxel(UVec3::new(x, h - 1, z), M_ROAD_DIRT);
        }
    }

    // Heli pad: 5×5 square at (HELI_PAD_X, HELI_PAD_Z).
    let h = terrain_height(HELI_PAD_X, HELI_PAD_Z);
    if h == 0 { return; }
    for dz in -2i32..=2 {
        for dx in -2i32..=2 {
            let x = (HELI_PAD_X as i32 + dx) as u32;
            let z = (HELI_PAD_Z as i32 + dz) as u32;
            let hh = terrain_height(x, z);
            if hh == 0 { continue; }
            set_voxel(UVec3::new(x, hh - 1, z), M_HELI_PAD);
        }
    }
}

/// Paint each cabin as a hollow box of cabin_wood with a pitched
/// cabin_roof on top. Cabins are anchored at their NW base corner;
/// the heights are sampled from the terrain at that corner so the
/// whole cabin sits flush.
fn paint_cabins() {
    for &(cx, cz) in &CABINS {
        let base = terrain_height(cx, cz);
        if base == 0 { continue; }
        // Foundation row of wood (so the cabin reads as built up).
        fill_box(
            UVec3::new(cx,          base, cz),
            UVec3::new(cx + CABIN_SX - 1, base, cz + CABIN_SZ - 1),
            M_CABIN_WOOD,
        );
        // Four walls. We fill the whole box then carve out the
        // interior — fewer host calls than building wall-by-wall.
        fill_box(
            UVec3::new(cx,          base + 1, cz),
            UVec3::new(cx + CABIN_SX - 1, base + CABIN_WALL_H, cz + CABIN_SZ - 1),
            M_CABIN_WOOD,
        );
        fill_box(
            UVec3::new(cx + 1,            base + 1, cz + 1),
            UVec3::new(cx + CABIN_SX - 2, base + CABIN_WALL_H, cz + CABIN_SZ - 2),
            0,
        );
        // Doorway on the south face: column at (cx + 3, cz + CABIN_SZ-1).
        let door_y0 = base + 1;
        let door_y1 = base + 3;
        fill_box(
            UVec3::new(cx + 3, door_y0, cz + CABIN_SZ - 1),
            UVec3::new(cx + 3, door_y1, cz + CABIN_SZ - 1),
            0,
        );
        // Roof slab — flat for now (CABIN_ROOF_H thick) so embers
        // landing on a cabin actually settle.
        fill_box(
            UVec3::new(cx,          base + CABIN_WALL_H + 1, cz),
            UVec3::new(cx + CABIN_SX - 1, base + CABIN_WALL_H + CABIN_ROOF_H, cz + CABIN_SZ - 1),
            M_CABIN_ROOF,
        );
    }
}

/// Drop ~250 pines on grass cells, weighted to the north half of the
/// footprint (the fire side). Avoids the lake, town, and road.
fn scatter_pines() {
    let mut rng = Rng::new(crate::scenario::get().forest_rng);
    let mut planted = 0u32;
    let mut tries = 0u32;
    // 500 pines × 9-cell canopy ≈ 12% leaf-coverage of the footprint.
    // At lower counts neighbouring trees rarely touched and fire
    // burnt itself out per tree without spreading.
    while planted < 500 && tries < 10_000 {
        tries += 1;
        // Bias north (lower z) a bit so the forest faces the fire.
        let bias_z = rng.unit() * (TOWN_Z as f32 - FOOT_MIN as f32 - 16.0);
        let tx = FOOT_MIN + 4 + rng.range(FOOT_LEN - 12);
        let tz = FOOT_MIN + 4 + (bias_z as u32).min(FOOT_LEN - 12);

        if forbidden_for_pine(tx, tz) { continue; }
        let h = terrain_height(tx, tz);
        if physics::material_at(tx, h - 1, tz) != M_GRASS { continue; }
        plant_pine(tx, tz, h, rng.next_u32());
        planted += 1;
    }
}

/// Pine: tall thin trunk + conical needle clusters. Lighter footprint
/// than big-world's broadleaf, so the forest doesn't look like a
/// solid hedge from the RTS overhead view.
fn plant_pine(cx: u32, cz: u32, base: u32, variant: u32) {
    let trunk_h = 5 + (variant % 3);  // 5, 6, or 7
    let trunk_top = base + trunk_h;
    fill_box(
        UVec3::new(cx, base, cz),
        UVec3::new(cx, trunk_top - 1, cz),
        M_PINE_WOOD,
    );
    let l0 = trunk_top - 2;
    let l1 = trunk_top - 1;
    let l2 = trunk_top;
    let l3 = trunk_top + 1;
    // 3×3 lower
    fill_box(UVec3::new(cx - 1, l0, cz - 1), UVec3::new(cx + 1, l0, cz + 1), M_PINE_LEAVES);
    // 3×3 mid
    fill_box(UVec3::new(cx - 1, l1, cz - 1), UVec3::new(cx + 1, l1, cz + 1), M_PINE_LEAVES);
    // 1×1 cross upper (just so the top reads as a needle cap)
    set_voxel(UVec3::new(cx, l2, cz), M_PINE_LEAVES);
    set_voxel(UVec3::new(cx - 1, l2, cz), M_PINE_LEAVES);
    set_voxel(UVec3::new(cx + 1, l2, cz), M_PINE_LEAVES);
    set_voxel(UVec3::new(cx, l2, cz - 1), M_PINE_LEAVES);
    set_voxel(UVec3::new(cx, l2, cz + 1), M_PINE_LEAVES);
    set_voxel(UVec3::new(cx, l3, cz), M_PINE_LEAVES);
}

fn forbidden_for_pine(x: u32, z: u32) -> bool {
    // Stay away from lake (12-cell margin), town strip (in TOWN_Z ±
    // CABIN_SZ + 4), and heli pad.
    let dx_lake = x as i32 - LAKE_CX as i32;
    let dz_lake = z as i32 - LAKE_CZ as i32;
    if dx_lake * dx_lake + dz_lake * dz_lake <= ((LAKE_R + 4) as i32).pow(2) {
        return true;
    }
    if x >= TOWN_MIN_X.saturating_sub(8) && x < TOWN_MAX_X + 8
        && z >= TOWN_Z.saturating_sub(20) && z < TOWN_Z + CABIN_SZ + 20
    {
        return true;
    }
    let dx_pad = x as i32 - HELI_PAD_X as i32;
    let dz_pad = z as i32 - HELI_PAD_Z as i32;
    if dx_pad.abs() <= 4 && dz_pad.abs() <= 4 { return true; }
    false
}

// ── Cabin survival check ──────────────────────────────────────────

/// Lightning strike: search for a flammable cell near the given XZ
/// and ignite it. Returns the ignited cell on success, or `None`
/// when the strike landed on bare grass / lake / road / town with
/// no flammable column nearby — the strike is wasted, no fire
/// starts. Caller (season.rs) accounts for the strike budget either
/// way, so a "dud" still counts.
pub(crate) fn strike_at(target_x: u32, target_z: u32) -> Option<UVec3> {
    // Scan a 16-cell box around the target XZ for any flammable cell
    // (canopy or trunk). The dy range only covers the realistic pine
    // canopy zone (terrain_height + 1..10) — pines are 5-8 voxels
    // tall, so anything above dy=10 is empty air, anything below is
    // ground.
    const RADIUS: i32 = 16;
    for dy in 1..10 {
        for dz in -RADIUS..=RADIUS {
            for dx in -RADIUS..=RADIUS {
                let x = (target_x as i32 + dx).clamp(0, 250) as u32;
                let z = (target_z as i32 + dz).clamp(0, 250) as u32;
                let y = (terrain_height(x, z) as i32 + dy).clamp(0, 250) as u32;
                let m = physics::material_at(x, y, z);
                if m == M_PINE_LEAVES || m == M_PINE_WOOD {
                    set_voxel(UVec3::new(x, y, z), M_FIRE);
                    return Some(UVec3::new(x, y, z));
                }
            }
        }
    }
    None
}

