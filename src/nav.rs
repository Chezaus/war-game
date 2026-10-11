//! Ground navigation (§6.2): the road network ("am I on a road?"), forest
//! lookup, and A* path finding on a NAV_CELL grid. Paths follow roads
//! (faster), avoid forests with vehicles (slower), and cross water only on
//! intact bridges.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bevy::prelude::*;

use crate::config::*;
use crate::geo;
use crate::world::{Bridges, Heightmap};

// ---------------------------------------------------------------------------
// Roads
// ---------------------------------------------------------------------------

/// Every road as a dense polyline (geo::road_polyline), with a coarse
/// spatial index of its segments.
#[derive(Resource)]
pub struct RoadNet {
    pub lines: Vec<Vec<Vec2>>,
    res: usize,
    /// Per NAV_CELL cell: (road, index of the segment's first point).
    index: Vec<Vec<(u16, u32)>>,
}

fn cell_of(p: Vec2, res: usize) -> Option<(usize, usize)> {
    let gx = ((p.x + WORLD_HALF) / NAV_CELL).floor();
    let gz = ((p.y + WORLD_HALF) / NAV_CELL).floor();
    (gx >= 0.0 && gz >= 0.0 && (gx as usize) < res && (gz as usize) < res).then(|| (gx as usize, gz as usize))
}

impl RoadNet {
    pub fn build() -> Self {
        let res = (WORLD_SIZE / NAV_CELL) as usize;
        let lines: Vec<Vec<Vec2>> = (0..geo::ROADS.len())
            .map(|r| geo::road_polyline(r).into_iter().map(Vec2::from).collect())
            .collect();
        let mut index = vec![Vec::new(); res * res];
        for (r, line) in lines.iter().enumerate() {
            for (i, s) in line.windows(2).enumerate() {
                let lo = s[0].min(s[1]) - Vec2::splat(ROAD_SNAP);
                let hi = s[0].max(s[1]) + Vec2::splat(ROAD_SNAP);
                let (Some(a), Some(b)) = (cell_of(lo, res), cell_of(hi, res)) else {
                    continue;
                };
                for gz in a.1..=b.1 {
                    for gx in a.0..=b.0 {
                        index[gz * res + gx].push((r as u16, i as u32));
                    }
                }
            }
        }
        Self { lines, res, index }
    }

    /// Closest point on any road within `max` meters.
    pub fn nearest(&self, p: Vec2, max: f32) -> Option<Vec2> {
        let (gx, gz) = cell_of(p, self.res)?;
        let mut best: Option<(Vec2, f32)> = None;
        for &(r, i) in &self.index[gz * self.res + gx] {
            let line = &self.lines[r as usize];
            let (a, b) = (line[i as usize], line[i as usize + 1]);
            let (d, t) = geo::seg_dist(p.into(), a.into(), b.into());
            if d < max && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((a.lerp(b, t), d));
            }
        }
        best.map(|(q, _)| q)
    }

    pub fn on_road(&self, p: Vec2) -> bool {
        self.nearest(p, ROAD_SNAP).is_some()
    }
}

// ---------------------------------------------------------------------------
// Navigation grid
// ---------------------------------------------------------------------------

/// Can a ground unit stand here? Dry land, or the deck of an intact bridge.
pub fn passable(hm: &Heightmap, bridges: &Bridges, p: Vec2) -> bool {
    !hm.is_water(p.x, p.y) || bridges.deck_at(p.x, p.y).is_some()
}

/// Movement speed factor from the ground under a unit (roads, forest).
pub fn terrain_speed(roads: &RoadNet, nav: &NavGrid, bridges: &Bridges, kind: UnitKind, p: Vec2) -> f32 {
    if roads.on_road(p) || bridges.deck_at(p.x, p.y).is_some() {
        ROAD_SPEED_FACTOR
    } else if nav.forest_at(p) {
        forest_speed(kind)
    } else {
        1.0
    }
}

pub fn forest_speed(kind: UnitKind) -> f32 {
    match kind {
        UnitKind::Infantry => FOREST_INFANTRY_SPEED,
        _ => FOREST_VEHICLE_SPEED,
    }
}

#[derive(Resource)]
pub struct NavGrid {
    pub res: usize,
    land: Vec<bool>,
    forest: Vec<bool>,
    /// Closest road point to the cell centre, if a road runs through it.
    road: Vec<Option<Vec2>>,
    /// Bridge crossing this cell, and the closest point on its deck line.
    bridge: Vec<Option<(u16, Vec2)>>,
    /// Connected regions (islands, given which bridges stand): a quick
    /// "unreachable" answer without a full search.
    comp: Vec<u32>,
    comp_key: (u32, usize),
}

impl NavGrid {
    pub fn build(hm: &Heightmap, roads: &RoadNet, bridges: &Bridges) -> Self {
        let res = (WORLD_SIZE / NAV_CELL) as usize;
        let mut land = vec![false; res * res];
        let mut forest = vec![false; res * res];
        let mut road = vec![None; res * res];
        let mut bridge = vec![None; res * res];
        // A 3×3 probe grid, so even a river slipping diagonally through
        // the corner between four cells closes them.
        let probe = NAV_CELL * 0.34;
        for gz in 0..res {
            for gx in 0..res {
                let i = gz * res + gx;
                let c = cell_center(gx, gz);
                land[i] = (-1..=1).all(|dz| (-1..=1).all(|dx| !hm.is_water(c.x + dx as f32 * probe, c.y + dz as f32 * probe)));
                if land[i] {
                    forest[i] = geo::forest_density(c.x, c.y, hm.height(c.x, c.y)) > FOREST_THRESHOLD;
                    road[i] = roads.nearest(c, NAV_CELL * 0.75).filter(|q| cell_of(*q, res) == Some((gx, gz)));
                }
            }
        }
        for (bi, b) in bridges.list.iter().enumerate() {
            // One cell past each end, so the deck always meets cells that
            // are dry enough to count as land.
            let (a, z) = (b.a - b.dir() * NAV_CELL, b.b + b.dir() * NAV_CELL);
            let n = (a.distance(z) / 25.0).ceil() as usize;
            let mut mark = |gx: usize, gz: usize| {
                let (_, t) = geo::seg_dist(cell_center(gx, gz).into(), b.a.into(), b.b.into());
                bridge[gz * res + gx] = Some((bi as u16, b.a.lerp(b.b, t)));
            };
            let mut last: Option<(usize, usize)> = None;
            for k in 0..=n {
                let Some((gx, gz)) = cell_of(a.lerp(z, k as f32 / n as f32), res) else {
                    continue;
                };
                // A diagonal step also takes both corner cells, so the deck
                // stays connected edge to edge.
                if let Some((lx, lz)) = last {
                    if lx != gx && lz != gz {
                        mark(gx, lz);
                        mark(lx, gz);
                    }
                }
                mark(gx, gz);
                last = Some((gx, gz));
            }
        }
        let mut nav = Self { res, land, forest, road, bridge, comp: Vec::new(), comp_key: (u32::MAX, 0) };
        nav.refresh_components(bridges);
        nav
    }

    /// Re-label connected regions if bridges changed since last time.
    pub fn refresh_components(&mut self, bridges: &Bridges) {
        let key = (bridges.generation, bridges.intact_count());
        if key == self.comp_key {
            return;
        }
        self.comp_key = key;
        let n = self.res;
        let mut comp = vec![u32::MAX; n * n];
        let mut next = 0;
        let mut stack = Vec::new();
        for s in 0..n * n {
            if comp[s] != u32::MAX || !self.open(s, bridges) {
                continue;
            }
            comp[s] = next;
            stack.push(s);
            while let Some(i) = stack.pop() {
                let (x, z) = (i % n, i / n);
                let neighbours = [
                    (x > 0).then(|| i - 1),
                    (x + 1 < n).then(|| i + 1),
                    (z > 0).then(|| i - n),
                    (z + 1 < n).then(|| i + n),
                ];
                for j in neighbours.into_iter().flatten() {
                    if comp[j] == u32::MAX && self.open(j, bridges) {
                        comp[j] = next;
                        stack.push(j);
                    }
                }
            }
            next += 1;
        }
        self.comp = comp;
    }

    /// Centre of the nearest dry cell (for placing units off rivers).
    pub fn nearest_land(&self, p: Vec2) -> Vec2 {
        let Some((gx, gz)) = cell_of(p, self.res) else {
            return p;
        };
        if self.land[gz * self.res + gx] {
            return p;
        }
        for r in 1..=8i32 {
            let mut best: Option<(Vec2, f32)> = None;
            for dz in -r..=r {
                for dx in -r..=r {
                    let (x, z) = (gx as i32 + dx, gz as i32 + dz);
                    if x < 0 || z < 0 || x >= self.res as i32 || z >= self.res as i32 || !self.land[z as usize * self.res + x as usize] {
                        continue;
                    }
                    let c = cell_center(x as usize, z as usize);
                    if best.is_none_or(|(_, d)| c.distance(p) < d) {
                        best = Some((c, c.distance(p)));
                    }
                }
            }
            if let Some((c, _)) = best {
                return c;
            }
        }
        p
    }

    pub fn forest_at(&self, p: Vec2) -> bool {
        cell_of(p, self.res).is_some_and(|(gx, gz)| self.forest[gz * self.res + gx])
    }

    fn open(&self, i: usize, bridges: &Bridges) -> bool {
        self.land[i] || self.bridge[i].is_some_and(|(b, _)| bridges.list[b as usize].intact)
    }

    /// Time-cost multiplier for entering cell `i`.
    fn cost(&self, i: usize, kind: UnitKind) -> f32 {
        if self.road[i].is_some() || self.bridge[i].is_some() {
            1.0 / ROAD_SPEED_FACTOR
        } else if self.forest[i] {
            1.0 / forest_speed(kind)
        } else {
            1.0
        }
    }

    /// Nearest open cell to `p` within a few cells.
    fn nearest_open(&self, p: Vec2, bridges: &Bridges) -> Option<usize> {
        let (gx, gz) = cell_of(p, self.res)?;
        let mut best: Option<(usize, f32)> = None;
        for r in 0..=4i32 {
            for dz in -r..=r {
                for dx in -r..=r {
                    let (x, z) = (gx as i32 + dx, gz as i32 + dz);
                    if x < 0 || z < 0 || x >= self.res as i32 || z >= self.res as i32 {
                        continue;
                    }
                    let i = z as usize * self.res + x as usize;
                    let d = cell_center(x as usize, z as usize).distance(p);
                    if self.open(i, bridges) && best.is_none_or(|(_, bd)| d < bd) {
                        best = Some((i, d));
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
        best.map(|(i, _)| i)
    }

    /// A* from `from` to `to`. Returns waypoints (ending at `to`), or None
    /// if water (or a destroyed bridge) is in the way.
    pub fn find_path(&self, hm: &Heightmap, roads: &RoadNet, bridges: &Bridges, from: Vec2, to: Vec2, kind: UnitKind) -> Option<Vec<Vec2>> {
        let start = self.nearest_open(from, bridges)?;
        let goal = self.nearest_open(to, bridges)?;
        let fresh = self.comp_key == (bridges.generation, bridges.intact_count());
        if fresh && self.comp[start] != self.comp[goal] {
            return None;
        }
        let cells = self.astar(start, goal, bridges, kind)?;
        let raw = self.waypoints(&cells, bridges, to);
        Some(smooth_path(hm, roads, bridges, from, &raw))
    }

    fn astar(&self, start: usize, goal: usize, bridges: &Bridges, kind: UnitKind) -> Option<Vec<usize>> {
        let n = self.res;
        let mut g = vec![f32::INFINITY; n * n];
        let mut came = vec![u32::MAX; n * n];
        let mut closed = vec![false; n * n];
        let mut heap = BinaryHeap::new();
        let (gx1, gz1) = ((goal % n) as f32, (goal / n) as f32);
        // Slightly greedy heuristic: much faster, paths still sensible.
        let h = |i: usize| ((i % n) as f32 - gx1).hypot((i / n) as f32 - gz1) * (1.3 / ROAD_SPEED_FACTOR);
        g[start] = 0.0;
        heap.push(Reverse((h(start).to_bits(), start)));
        const DIRS: [(i32, i32, f32); 8] = [
            (1, 0, 1.0),
            (-1, 0, 1.0),
            (0, 1, 1.0),
            (0, -1, 1.0),
            (1, 1, std::f32::consts::SQRT_2),
            (1, -1, std::f32::consts::SQRT_2),
            (-1, 1, std::f32::consts::SQRT_2),
            (-1, -1, std::f32::consts::SQRT_2),
        ];
        while let Some(Reverse((_, i))) = heap.pop() {
            if i == goal {
                let mut path = vec![goal];
                let mut c = goal;
                while c != start {
                    c = came[c] as usize;
                    path.push(c);
                }
                path.reverse();
                return Some(path);
            }
            if closed[i] {
                continue;
            }
            closed[i] = true;
            let (x, z) = ((i % n) as i32, (i / n) as i32);
            for (dx, dz, len) in DIRS {
                let (nx, nz) = (x + dx, z + dz);
                if nx < 0 || nz < 0 || nx >= n as i32 || nz >= n as i32 {
                    continue;
                }
                let j = nz as usize * n + nx as usize;
                if closed[j] || !self.open(j, bridges) {
                    continue;
                }
                // No cutting corners past water.
                if dx != 0 && dz != 0 && (!self.open(z as usize * n + nx as usize, bridges) || !self.open(nz as usize * n + x as usize, bridges)) {
                    continue;
                }
                let ng = g[i] + len * 0.5 * (self.cost(i, kind) + self.cost(j, kind));
                if ng < g[j] {
                    g[j] = ng;
                    came[j] = i as u32;
                    heap.push(Reverse(((ng + h(j)).to_bits(), j)));
                }
            }
        }
        None
    }

    /// Cells → points: snapped onto bridges and roads, with both ends of
    /// every bridge as explicit waypoints so units drive straight across.
    /// Cells → points, snapped onto bridges and roads. Both ends of every
    /// bridge crossed become waypoints that smoothing must keep (`true`),
    /// so units drive straight across the deck.
    fn waypoints(&self, cells: &[usize], bridges: &Bridges, to: Vec2) -> Vec<(Vec2, bool)> {
        // Runs of cells on the same intact bridge count as a crossing only
        // if they include water (not just a bridge head on the bank).
        let ids: Vec<Option<u16>> = cells
            .iter()
            .map(|&c| self.bridge[c].filter(|(b, _)| bridges.list[*b as usize].intact).map(|(b, _)| b))
            .collect();
        let mut crossing = vec![false; cells.len()];
        let mut k = 0;
        while k < cells.len() {
            let Some(b) = ids[k] else {
                k += 1;
                continue;
            };
            let start = k;
            while k < cells.len() && ids[k] == Some(b) {
                k += 1;
            }
            if (start..k).any(|m| !self.land[cells[m]]) {
                crossing[start..k].fill(true);
            }
        }

        let mut out: Vec<(Vec2, bool)> = Vec::new();
        let mut on_bridge: Option<u16> = None;
        let mut prev = cell_center(cells[0] % self.res, cells[0] / self.res);
        // Where we stood before driving onto the current bridge.
        let mut entry = prev;
        for (k, &c) in cells.iter().enumerate().skip(1) {
            let here = if crossing[k] { ids[k] } else { None };
            if here != on_bridge {
                if let Some(b) = on_bridge {
                    out.push((bridges.list[b as usize].far_end(entry), true));
                }
                if let Some(b) = here {
                    entry = prev;
                    out.push((bridges.list[b as usize].near_end(entry), true));
                }
                on_bridge = here;
            }
            let p = match (here, self.bridge[c]) {
                (Some(_), Some((_, q))) => q,
                _ => self.road[c].unwrap_or(cell_center(c % self.res, c / self.res)),
            };
            out.push((p, false));
            prev = p;
        }
        if let Some(b) = on_bridge {
            out.push((bridges.list[b as usize].far_end(entry), true));
        } else if !out.is_empty() {
            // The last cell's point is replaced by the exact destination.
            out.pop();
        }
        out.push((to, true));
        out
    }
}

fn cell_center(gx: usize, gz: usize) -> Vec2 {
    Vec2::new(-WORLD_HALF + (gx as f32 + 0.5) * NAV_CELL, -WORLD_HALF + (gz as f32 + 0.5) * NAV_CELL)
}

/// Is the straight line a→b walkable? Sampled every 20 m. Lines between
/// two road points must also stay on the road, so smoothing never trades
/// a fast road for a cross-country shortcut.
fn line_ok(hm: &Heightmap, roads: &RoadNet, bridges: &Bridges, a: Vec2, b: Vec2) -> bool {
    let on_road = |p: Vec2| roads.on_road(p) || bridges.deck_at(p.x, p.y).is_some();
    let road_mode = on_road(a) && on_road(b);
    let n = (a.distance(b) / 20.0).ceil().max(1.0) as usize;
    (1..n).all(|k| {
        let p = a.lerp(b, k as f32 / n as f32);
        passable(hm, bridges, p) && (!road_mode || on_road(p))
    })
}

/// String pulling: skip waypoints while the straight line stays walkable,
/// but never past one marked `keep` (bridge ends, the destination).
fn smooth_path(hm: &Heightmap, roads: &RoadNet, bridges: &Bridges, from: Vec2, raw: &[(Vec2, bool)]) -> Vec<Vec2> {
    let mut out = Vec::new();
    let mut anchor = from;
    let mut i = 0;
    while i < raw.len() {
        let mut best = i;
        while best + 1 < raw.len()
            && !raw[best].1
            && best + 1 < i + 40
            && anchor.distance(raw[best + 1].0) < 3_000.0
            && line_ok(hm, roads, bridges, anchor, raw[best + 1].0)
        {
            best += 1;
        }
        out.push(raw[best].0);
        anchor = raw[best].0;
        i = best + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heightgen;

    fn world() -> (Heightmap, RoadNet, Bridges, NavGrid) {
        let hm = Heightmap::from_heights(HEIGHTMAP_RES, heightgen::generate());
        let roads = RoadNet::build();
        let bridges = Bridges::build(&hm);
        let nav = NavGrid::build(&hm, &roads, &bridges);
        (hm, roads, bridges, nav)
    }

    #[test]
    fn navigation_on_the_mini_earth() {
        let (hm, roads, mut bridges, nav) = world();
        let walkable = |bridges: &Bridges, from: Vec2, path: &[Vec2]| {
            let mut a = from;
            for &b in path {
                let n = (a.distance(b) / 10.0).ceil() as usize;
                for k in 1..n {
                    let p = a.lerp(b, k as f32 / n as f32);
                    assert!(passable(&hm, bridges, p), "path crosses water at {p}");
                }
                a = b;
            }
        };
        // Belarus to Poland: over the Neman and Vistula bridges.
        let w = Vec2::from(geo::geo(19.6, 52.4));
        let e = Vec2::from(geo::geo(27.2, 53.6));
        let path = nav.find_path(&hm, &roads, &bridges, e, w, UnitKind::Armor).expect("Belarus to Poland");
        assert_eq!(*path.last().unwrap(), w);
        walkable(&bridges, e, &path);
        // Britain is only reachable over the Channel bridge.
        let london = Vec2::from(geo::geo(-0.5, 51.8));
        let paris = Vec2::from(geo::geo(2.9, 48.4)); // south-east of Paris, off the Seine
        assert!(!hm.is_water(paris.x, paris.y));
        let path = nav.find_path(&hm, &roads, &bridges, paris, london, UnitKind::Infantry).expect("Paris to London");
        walkable(&bridges, paris, &path);
        for b in &mut bridges.list {
            b.intact = false;
        }
        let mut nav = nav;
        nav.refresh_components(&bridges);
        assert!(nav.find_path(&hm, &roads, &bridges, paris, london, UnitKind::Infantry).is_none());
    }

    #[test]
    fn roads_are_found() {
        let roads = RoadNet::build();
        let warsaw = Vec2::from(geo::geo_ll(geo::WARSAW));
        assert!(roads.on_road(warsaw));
        assert!(!roads.on_road(warsaw + Vec2::new(2_000.0, 1_700.0)));
    }
}
