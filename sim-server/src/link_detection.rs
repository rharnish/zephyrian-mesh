// Ported from cesium-app/src/linkDetection.js. `compute_grid_edges` is the
// production path (spatial grid + precomputed trig); `brute_force_edge_keys`
// is the O(n^2) ground-truth oracle used only in tests, mirroring
// linkDetection.js's verifyEdgesOnce pattern.

use crate::balloon::Balloon;
use crate::config::BALLOON_MAX_ALT;
use crate::geo::{horizon_km, in_radio_range_precomputed, precompute, Precomputed};
use crate::spatial_grid::SpatialGrid;
use crate::tower::Tower;
use std::collections::{HashMap, HashSet};

struct NodeInfo {
    key: String,
    lon: f64,
    lat: f64,
    pre: Precomputed,
}

fn pair_key_for(a_key: &str, b_key: &str) -> String {
    if a_key < b_key {
        format!("{a_key}|{b_key}")
    } else {
        format!("{b_key}|{a_key}")
    }
}

fn build_nodes(balloons: &[Balloon], towers: &[Tower], horizon_refraction_coeff: f64) -> Vec<NodeInfo> {
    let mut nodes = Vec::with_capacity(balloons.len() + towers.len());
    for b in balloons {
        nodes.push(NodeInfo {
            key: format!("b{}", b.id),
            lon: b.lon,
            lat: b.lat,
            pre: precompute(b.lon, b.lat, b.alt, horizon_refraction_coeff),
        });
    }
    for t in towers {
        nodes.push(NodeInfo {
            key: format!("t{}", t.id),
            lon: t.lon,
            lat: t.lat,
            pre: precompute(t.lon, t.lat, t.height_m, horizon_refraction_coeff),
        });
    }
    nodes
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub a_key: String,
    pub b_key: String,
}

/// The production edge finder: spatial grid + precomputed trig.
pub fn compute_grid_edges(
    balloons: &[Balloon],
    towers: &[Tower],
    grid: &mut SpatialGrid,
    max_range_km: f64,
    horizon_refraction_coeff: f64,
) -> Vec<Edge> {
    let nodes = build_nodes(balloons, towers, horizon_refraction_coeff);

    grid.clear();
    for (i, n) in nodes.iter().enumerate() {
        grid.insert(i, n.lon, n.lat);
    }

    let mut edges_by_pair: HashMap<String, Edge> = HashMap::new();
    let mut seen_pairs: HashSet<String> = HashSet::new();

    for (i, node) in nodes.iter().enumerate() {
        let candidates = grid.neighbors(node.lon, node.lat, max_range_km);
        for &j in &candidates {
            if j == i {
                continue;
            }
            let other = &nodes[j];
            let pair_key = pair_key_for(&node.key, &other.key);
            if seen_pairs.contains(&pair_key) {
                continue;
            }
            seen_pairs.insert(pair_key.clone());

            if in_radio_range_precomputed(&node.pre, &other.pre) {
                edges_by_pair.insert(
                    pair_key,
                    Edge { a_key: node.key.clone(), b_key: other.key.clone() },
                );
            }
        }
    }

    edges_by_pair.into_values().collect()
}

/// Ground truth: O(n^2), no spatial grid — checks every pair. Slow, but
/// correct by construction; used as a test oracle against compute_grid_edges.
pub fn brute_force_edge_keys(balloons: &[Balloon], towers: &[Tower], horizon_refraction_coeff: f64) -> HashSet<String> {
    let nodes = build_nodes(balloons, towers, horizon_refraction_coeff);
    let mut keys = HashSet::new();
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            if in_radio_range_precomputed(&nodes[i].pre, &nodes[j].pre) {
                keys.insert(pair_key_for(&nodes[i].key, &nodes[j].key));
            }
        }
    }
    keys
}

// Array-based union-find over plain node indices (0..n) — no string keys,
// no heap allocation, no hashing of anything but a packed u64. This is the
// perf-critical counterpart to UnionFind (union_find.rs), which is
// string-keyed and fine for the live app's throttled (every-3rd-tick)
// link recompute at a few hundred balloons, but was the dominant cost when
// reused as-is in a per-simulated-second loop over 24 sim-hours x 96 sweep
// combos (see the connectivity_sweep vs. connectivity-sweep.mjs wall-clock
// comparison — this is the change that actually made Rust faster than the
// JS port, not the language itself).
fn find(parent: &mut [u32], x: u32) -> u32 {
    let mut root = x;
    while parent[root as usize] != root {
        root = parent[root as usize];
    }
    let mut cur = x;
    while parent[cur as usize] != root {
        let next = parent[cur as usize];
        parent[cur as usize] = root;
        cur = next;
    }
    root
}

fn union(parent: &mut [u32], a: u32, b: u32) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra != rb {
        parent[ra as usize] = rb;
    }
}

/// Ported from connectivity-sweep.mjs's computeGroundedBalloonIds: which
/// balloon ids belong to a connected component that includes at least one
/// tower. Nodes are addressed by plain index here (balloons first, towers
/// after) rather than by string key — see the `find`/`union` comment above
/// for why. Balloon index == `Balloon.id` is relied on (true for how
/// connectivity_sweep constructs its balloon Vec: sequential ids 0..n in
/// push order).
pub fn grounded_balloon_ids(
    balloons: &[Balloon],
    towers: &[Tower],
    grid: &mut SpatialGrid,
    horizon_refraction_coeff: f64,
) -> HashSet<u32> {
    let n_balloons = balloons.len();
    let n = n_balloons + towers.len();

    let mut lons = Vec::with_capacity(n);
    let mut lats = Vec::with_capacity(n);
    let mut pre = Vec::with_capacity(n);
    for b in balloons {
        lons.push(b.lon);
        lats.push(b.lat);
        pre.push(precompute(b.lon, b.lat, b.alt, horizon_refraction_coeff));
    }
    for t in towers {
        lons.push(t.lon);
        lats.push(t.lat);
        pre.push(precompute(t.lon, t.lat, t.height_m, horizon_refraction_coeff));
    }

    grid.clear();
    for i in 0..n {
        grid.insert(i, lons[i], lats[i]);
    }

    let max_range_km = 2.0 * horizon_km(BALLOON_MAX_ALT, horizon_refraction_coeff);
    let mut parent: Vec<u32> = (0..n as u32).collect();

    // Pack the pair (i, j) into one u64 for the seen-pairs set — avoids
    // formatting/hashing strings for every candidate check, which is what
    // made the straightforward (String-keyed) port no faster than the JS
    // version despite being compiled/native.
    let mut seen: HashSet<u64> = HashSet::with_capacity(n * 8);
    for i in 0..n {
        let candidates = grid.neighbors(lons[i], lats[i], max_range_km);
        for &j in &candidates {
            if j == i {
                continue;
            }
            let (a, b) = if i < j { (i, j) } else { (j, i) };
            let key = ((a as u64) << 32) | (b as u64);
            if !seen.insert(key) {
                continue;
            }
            if in_radio_range_precomputed(&pre[i], &pre[j]) {
                union(&mut parent, i as u32, j as u32);
            }
        }
    }

    let mut grounded_roots: HashSet<u32> = HashSet::new();
    for ti in n_balloons..n {
        grounded_roots.insert(find(&mut parent, ti as u32));
    }
    let mut grounded_ids = HashSet::new();
    for (bi, b) in balloons.iter().enumerate() {
        if grounded_roots.contains(&find(&mut parent, bi as u32)) {
            grounded_ids.insert(b.id);
        }
    }
    grounded_ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    #[test]
    fn grid_matches_brute_force_ground_truth() {
        let mut rng = StdRng::seed_from_u64(7);
        let horizon_coeff = 4.12;

        // Full lat range, poles included — spatial_grid.rs widens its
        // longitude search window near the poles specifically so this holds
        // everywhere, not just away from the poles (see
        // pole_wraparound_edges_are_not_missed below for a dedicated repro).
        let balloons: Vec<Balloon> = (0..300)
            .map(|i| {
                let lon = rng.gen::<f64>() * 360.0 - 180.0;
                let lat = rng.gen::<f64>() * 180.0 - 90.0;
                let alt = 15000.0 + rng.gen::<f64>() * 10000.0;
                Balloon::new(i, lon, lat, alt)
            })
            .collect();
        let towers: Vec<Tower> = (0..12)
            .map(|i| {
                let lon = rng.gen::<f64>() * 360.0 - 180.0;
                let lat = rng.gen::<f64>() * 180.0 - 90.0;
                Tower::new(i, lon, lat, 30.0)
            })
            .collect();

        let max_range_km = 2.0 * crate::geo::horizon_km(25000.0, horizon_coeff);
        let mut grid = SpatialGrid::new(6.0);
        let grid_edges = compute_grid_edges(&balloons, &towers, &mut grid, max_range_km, horizon_coeff);
        let grid_keys: HashSet<String> = grid_edges
            .iter()
            .map(|e| pair_key_for(&e.a_key, &e.b_key))
            .collect();
        let ground_truth = brute_force_edge_keys(&balloons, &towers, horizon_coeff);

        let missed: Vec<_> = ground_truth.difference(&grid_keys).collect();
        let spurious: Vec<_> = grid_keys.difference(&ground_truth).collect();
        assert!(missed.is_empty(), "grid algorithm missed real edges: {missed:?}");
        assert!(spurious.is_empty(), "grid algorithm found edges that aren't real: {spurious:?}");
    }

    // Regression test for a gap found while porting spatialGrid.js: two
    // nodes on opposite sides of a pole can be physically close (small
    // great-circle distance) while differing in longitude by ~180 degrees.
    // A cosine-scaled longitude window has no notion of wrapping over the
    // pole, so it used to miss real in-range pairs there. Fixed in
    // spatial_grid.rs::neighbors (widens to a full 360-degree sweep when the
    // search radius reaches the pole). This exact seed/count reproduced the
    // bug before the fix; kept as a named regression test.
    #[test]
    fn pole_wraparound_edges_are_not_missed() {
        let mut rng = StdRng::seed_from_u64(7);
        let horizon_coeff = 4.12;
        let balloons: Vec<Balloon> = (0..300)
            .map(|i| {
                let lon = rng.gen::<f64>() * 360.0 - 180.0;
                let lat = rng.gen::<f64>() * 180.0 - 90.0;
                let alt = 15000.0 + rng.gen::<f64>() * 10000.0;
                Balloon::new(i, lon, lat, alt)
            })
            .collect();
        let towers: Vec<Tower> = Vec::new();

        let max_range_km = 2.0 * crate::geo::horizon_km(25000.0, horizon_coeff);
        let mut grid = SpatialGrid::new(6.0);
        let grid_edges = compute_grid_edges(&balloons, &towers, &mut grid, max_range_km, horizon_coeff);
        let grid_keys: HashSet<String> =
            grid_edges.iter().map(|e| pair_key_for(&e.a_key, &e.b_key)).collect();
        let ground_truth = brute_force_edge_keys(&balloons, &towers, horizon_coeff);
        let missed: Vec<_> = ground_truth.difference(&grid_keys).collect();
        assert!(missed.is_empty(), "reproduces the polar gap: {missed:?}");
    }
}
