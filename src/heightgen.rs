//! Procedural heightmap for the mini Earth (geo.rs): rasterised coastlines
//! turned into a signed distance field, rolling inland terrain, mountain
//! ranges, river valleys, straits and two flattened runway areas.
//!
//! Used by `examples/gen_heightmap.rs` to paint `assets/heightmap.png`,
//! and by the game as a fallback if the PNG is missing. Pure std, no Bevy.

#![allow(dead_code)]

use crate::config::*;
use crate::geo::{self, fbm, lerp, seg_dist, world_pts};
pub use crate::geo::smoothstep;

struct Grid {
    res: usize,
    cell: f32,
}

impl Grid {
    fn new() -> Self {
        Self { res: HEIGHTMAP_RES, cell: WORLD_SIZE / (HEIGHTMAP_RES - 1) as f32 }
    }
    fn pos(&self, ix: usize, iz: usize) -> [f32; 2] {
        [-WORLD_HALF + ix as f32 * self.cell, -WORLD_HALF + iz as f32 * self.cell]
    }
    /// Index range of samples within `r` of the box [x0, x1] × [z0, z1].
    fn span(&self, x0: f32, x1: f32, z0: f32, z1: f32, r: f32) -> (usize, usize, usize, usize) {
        let to = |v: f32| ((v + WORLD_HALF) / self.cell).clamp(0.0, (self.res - 1) as f32);
        (
            to(x0 - r).floor() as usize,
            to(x1 + r).ceil() as usize,
            to(z0 - r).floor() as usize,
            to(z1 + r).ceil() as usize,
        )
    }
}

/// Land mask by scanline: inside any land ring and no sea ring.
fn land_mask(g: &Grid) -> Vec<bool> {
    let land: Vec<Vec<[f32; 2]>> = geo::LAND.iter().map(|r| world_pts(r)).collect();
    let seas: Vec<Vec<[f32; 2]>> = geo::SEAS.iter().map(|r| world_pts(r)).collect();
    let mut mask = vec![false; g.res * g.res];
    let fill = |rings: &[Vec<[f32; 2]>], value: bool, mask: &mut Vec<bool>| {
        for ring in rings {
            let (zmin, zmax) = ring.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
            let (_, _, iz0, iz1) = g.span(0.0, 0.0, zmin, zmax, 0.0);
            let mut xs = Vec::new();
            for iz in iz0..=iz1 {
                let z = g.pos(0, iz)[1];
                xs.clear();
                for k in 0..ring.len() {
                    let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
                    if (a[1] <= z) != (b[1] <= z) {
                        xs.push(a[0] + (z - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
                    }
                }
                xs.sort_by(f32::total_cmp);
                for pair in xs.chunks_exact(2) {
                    let (ix0, ix1, _, _) = g.span(pair[0], pair[1], 0.0, 0.0, 0.0);
                    for ix in ix0..=ix1 {
                        let x = g.pos(ix, 0)[0];
                        if x >= pair[0] && x <= pair[1] {
                            mask[iz * g.res + ix] = value;
                        }
                    }
                }
            }
        }
    };
    fill(&land, true, &mut mask);
    fill(&seas, false, &mut mask);
    mask
}

/// Two-pass chamfer distance (meters) from every cell to the nearest cell
/// where `target` is true.
fn distance_to(g: &Grid, target: &[bool]) -> Vec<f32> {
    let n = g.res;
    let (orth, diag) = (g.cell, g.cell * std::f32::consts::SQRT_2);
    let mut d: Vec<f32> = target.iter().map(|&t| if t { 0.0 } else { 1e9 }).collect();
    for iz in 0..n {
        for ix in 0..n {
            let i = iz * n + ix;
            let mut v = d[i];
            if ix > 0 {
                v = v.min(d[i - 1] + orth);
            }
            if iz > 0 {
                v = v.min(d[i - n] + orth);
                if ix > 0 {
                    v = v.min(d[i - n - 1] + diag);
                }
                if ix + 1 < n {
                    v = v.min(d[i - n + 1] + diag);
                }
            }
            d[i] = v;
        }
    }
    for iz in (0..n).rev() {
        for ix in (0..n).rev() {
            let i = iz * n + ix;
            let mut v = d[i];
            if ix + 1 < n {
                v = v.min(d[i + 1] + orth);
            }
            if iz + 1 < n {
                v = v.min(d[i + n] + orth);
                if ix + 1 < n {
                    v = v.min(d[i + n + 1] + diag);
                }
                if ix > 0 {
                    v = v.min(d[i + n - 1] + diag);
                }
            }
            d[i] = v;
        }
    }
    d
}

/// Mountain height field: the highest Gaussian ridge contribution per cell.
fn ridge_field(g: &Grid) -> Vec<f32> {
    let mut out = vec![0.0f32; g.res * g.res];
    for r in geo::RIDGES {
        let pts = geo::densify(&world_pts(r.pts), 400.0, 0.0, 0);
        let reach = r.width * 2.6;
        for s in pts.windows(2) {
            let (a, b) = (s[0], s[1]);
            let (ix0, ix1, iz0, iz1) = g.span(a[0].min(b[0]), a[0].max(b[0]), a[1].min(b[1]), a[1].max(b[1]), reach);
            for iz in iz0..=iz1 {
                for ix in ix0..=ix1 {
                    let d = seg_dist(g.pos(ix, iz), a, b).0;
                    if d < reach {
                        let v = r.height * (-(d / r.width).powi(2)).exp();
                        let o = &mut out[iz * g.res + ix];
                        *o = o.max(v);
                    }
                }
            }
        }
    }
    out
}

/// Nearest waterway per cell: (distance, half width), split into rivers
/// and straits.
fn water_fields(g: &Grid) -> (Vec<(f32, f32)>, Vec<(f32, f32)>) {
    let mut rivers = vec![(f32::MAX, 0.0f32); g.res * g.res];
    let mut straits = vec![(f32::MAX, 0.0f32); g.res * g.res];
    for (i, w) in geo::WATERWAYS.iter().enumerate() {
        let pts = geo::waterway_polyline(i);
        let reach = if w.strait { w.half_width + 400.0 } else { w.half_width + RIVER_VALLEY_MAX };
        let field = if w.strait { &mut straits } else { &mut rivers };
        for s in pts.windows(2) {
            let (a, b) = (s[0], s[1]);
            let (ix0, ix1, iz0, iz1) = g.span(a[0].min(b[0]), a[0].max(b[0]), a[1].min(b[1]), a[1].max(b[1]), reach);
            for iz in iz0..=iz1 {
                for ix in ix0..=ix1 {
                    let d = seg_dist(g.pos(ix, iz), a, b).0;
                    let o = &mut field[iz * g.res + ix];
                    // Compare by distance to the water's edge, so a wide
                    // river wins over a narrow one at the same distance.
                    if d - w.half_width < o.0 - o.1 {
                        *o = (d, w.half_width);
                    }
                }
            }
        }
    }
    (rivers, straits)
}

/// Row-major grid of HEIGHTMAP_RES² heights; sample (ix, iz) sits at
/// x = -WORLD_HALF + ix * cell, z = -WORLD_HALF + iz * cell.
pub fn generate() -> Vec<f32> {
    let g = Grid::new();
    let mask = land_mask(&g);
    let water: Vec<bool> = mask.iter().map(|&l| !l).collect();
    let to_water = distance_to(&g, &water);
    let to_land = distance_to(&g, &mask);
    let ridges = ridge_field(&g);
    let (rivers, straits) = water_fields(&g);

    let mut out = Vec::with_capacity(g.res * g.res);
    for iz in 0..g.res {
        for ix in 0..g.res {
            let i = iz * g.res + ix;
            let [x, z] = g.pos(ix, iz);
            // Signed distance to the coast (+ inland), roughened by noise.
            let sdf = if mask[i] { to_water[i] - g.cell * 0.5 } else { g.cell * 0.5 - to_land[i] };
            let s = sdf + (fbm(x / 2_600.0, z / 2_600.0, 4, 7) - 0.5) * 2.0 * 280.0
                + (fbm(x / 700.0, z / 700.0, 3, 8) - 0.5) * 2.0 * 60.0;
            let detail = fbm(x / 650.0, z / 650.0, 3, 5) * 14.0;
            let mut h = if s <= 0.0 {
                (s * 0.02).max(-70.0) - 3.0
            } else {
                let inland = s / 1_000.0;
                let base = 2.0 + 40.0 * (1.0 - (-inland / 1.2).exp()) + 5.0 * inland.min(30.0);
                let rugged = smoothstep(0.5, 0.75, fbm(x / 12_000.0, z / 12_000.0, 3, 11));
                let hills = fbm(x / 2_500.0, z / 2_500.0, 4, 1).powi(2) * (50.0 + 300.0 * rugged);
                let crest = 1.0 - (fbm(x / 2_800.0, z / 2_800.0, 3, 3) * 2.0 - 1.0).abs();
                let mountains = ridges[i] * (0.65 + 0.7 * crest);
                let land = base + hills + detail + mountains;
                lerp(0.6, land, smoothstep(0.0, 500.0, s))
            };

            // River valleys.
            let (d, hw) = rivers[i];
            if h > 0.0 && d < hw + RIVER_VALLEY_MAX {
                let valley = (hw + RIVER_VALLEY_BASE + h * RIVER_VALLEY_PER_M).min(RIVER_VALLEY_MAX);
                if d < hw {
                    h = -5.0 - 3.0 * (1.0 - d / hw);
                } else if d < valley {
                    let bank = 1.5 + detail * 0.05;
                    h = h.min(lerp(bank, h, smoothstep(hw + 60.0, valley, d)));
                }
            }
            // Straits: open water straight through.
            let (d, hw) = straits[i];
            if d < hw {
                h = h.min(-9.0);
            } else if d < hw + 250.0 {
                h = h.min(lerp(-9.0, h.max(0.0), smoothstep(hw, hw + 250.0, d)));
            }

            // Airbases: gentle base zone, then a perfectly flat runway area.
            for base in [NATO_BASE, ENEMY_BASE] {
                let (dx, dz) = ((x - base.center[0]).abs(), (z - base.center[1]).abs());
                let dist = (dx * dx + dz * dz).sqrt();
                let gentle = smoothstep(BASE_GENTLE_RADIUS * 0.4, BASE_GENTLE_RADIUS, dist);
                h = lerp(base.elevation + (h - base.elevation) * 0.15, h, gentle);
                let qx = (dx - RUNWAY_FLAT_HALF_X).max(0.0);
                let qz = (dz - RUNWAY_FLAT_HALF_Z).max(0.0);
                let q = (qx * qx + qz * qz).sqrt();
                let w = 1.0 - smoothstep(0.0, RUNWAY_FLAT_BLEND, q);
                h = lerp(h, base.elevation, w);
            }
            out.push(h.clamp(HEIGHT_MIN, HEIGHT_MAX));
        }
    }
    out
}

pub fn encode(h: f32) -> u16 {
    (((h - HEIGHT_MIN) / (HEIGHT_MAX - HEIGHT_MIN)).clamp(0.0, 1.0) * 65_535.0).round() as u16
}

pub fn decode(v: u16) -> f32 {
    HEIGHT_MIN + v as f32 / 65_535.0 * (HEIGHT_MAX - HEIGHT_MIN)
}
