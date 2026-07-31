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

/// Identifies a balloon or tower in the connectivity graph. A tagged id
/// rather than a `format!("b{id}")` / `format!("t{id}")` string — the graph
/// gets rebuilt every link-recompute tick, so this is a Copy value with no
/// per-node allocation, and the balloon/tower distinction is enforced by the
/// type rather than by convention (no risk of a typo'd prefix or a collision
/// between a balloon id and a tower id that happen to match).
///
/// Converted to the `"b{id}"` / `"t{id}"` wire strings only at the snapshot
/// boundary (`wire_pair_key`, used by sim.rs) — the JS frontend parses that
/// format (see `parseNodeKey` in linkLayer.js), so it has to stay stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKey {
    Balloon(u32),
    Tower(u32),
}

impl std::fmt::Display for NodeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeKey::Balloon(id) => write!(f, "b{id}"),
            NodeKey::Tower(id) => write!(f, "t{id}"),
        }
    }
}

/// The wire-format pair key clients key their rendered links by (see
/// linkLayer.js). Order is arbitrary but must be deterministic for a given
/// unordered pair, since both directions of the same edge must serialize
/// identically.
pub fn wire_pair_key(a: NodeKey, b: NodeKey) -> String {
    if a < b {
        format!("{a}|{b}")
    } else {
        format!("{b}|{a}")
    }
}

fn ordered_pair(a: NodeKey, b: NodeKey) -> (NodeKey, NodeKey) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

struct NodeInfo {
    key: NodeKey,
    lon: f64,
    lat: f64,
    pre: Precomputed,
}

fn build_nodes(balloons: &[Balloon], towers: &[Tower], horizon_refraction_coeff: f64) -> Vec<NodeInfo> {
    let mut nodes = Vec::with_capacity(balloons.len() + towers.len());
    for b in balloons {
        nodes.push(NodeInfo {
            key: NodeKey::Balloon(b.id),
            lon: b.lon,
            lat: b.lat,
            pre: precompute(b.lon, b.lat, b.alt, horizon_refraction_coeff),
        });
    }
    for t in towers {
        nodes.push(NodeInfo {
            key: NodeKey::Tower(t.id),
            lon: t.lon,
            lat: t.lat,
            pre: precompute(t.lon, t.lat, t.height_m, horizon_refraction_coeff),
        });
    }
    nodes
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub a: NodeKey,
    pub b: NodeKey,
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

    let mut edges_by_pair: HashMap<(NodeKey, NodeKey), Edge> = HashMap::new();
    let mut seen_pairs: HashSet<(NodeKey, NodeKey)> = HashSet::new();

    for (i, node) in nodes.iter().enumerate() {
        let candidates = grid.neighbors(node.lon, node.lat, max_range_km);
        for &j in &candidates {
            if j == i {
                continue;
            }
            let other = &nodes[j];
            let pair = ordered_pair(node.key, other.key);
            if !seen_pairs.insert(pair) {
                continue;
            }

            if in_radio_range_precomputed(&node.pre, &other.pre) {
                edges_by_pair.insert(pair, Edge { a: node.key, b: other.key });
            }
        }
    }

    edges_by_pair.into_values().collect()
}

/// Ground truth: O(n^2), no spatial grid — checks every pair. Slow, but
/// correct by construction; used as a test oracle against compute_grid_edges.
pub fn brute_force_edge_keys(
    balloons: &[Balloon],
    towers: &[Tower],
    horizon_refraction_coeff: f64,
) -> HashSet<(NodeKey, NodeKey)> {
    let nodes = build_nodes(balloons, towers, horizon_refraction_coeff);
    let mut keys = HashSet::new();
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            if in_radio_range_precomputed(&nodes[i].pre, &nodes[j].pre) {
                keys.insert(ordered_pair(nodes[i].key, nodes[j].key));
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
///
/// This computation runs once per simulated second, unthrottled, over a
/// 24-sim-hour sweep — so per-call allocation overhead (rebuilding buffers
/// sized for n nodes from scratch every call) dominates wall-clock time at
/// scale. `ConnectivityScratch` owns those buffers across calls instead of
/// reallocating them each time; the computation itself (grid contents,
/// union-find, grounded-set membership) is unchanged from a stateless
/// version, so results are identical, only the allocator traffic differs.
pub struct ConnectivityScratch {
    lons: Vec<f64>,
    lats: Vec<f64>,
    pre: Vec<Precomputed>,
    parent: Vec<u32>,
    seen: HashSet<u64>,
    grounded_roots: HashSet<u32>,
    grounded_ids: HashSet<u32>,
}

impl ConnectivityScratch {
    pub fn new(n_balloons: usize, n_towers: usize) -> Self {
        let n = n_balloons + n_towers;
        ConnectivityScratch {
            lons: vec![0.0; n],
            lats: vec![0.0; n],
            pre: Vec::with_capacity(n),
            parent: (0..n as u32).collect(),
            seen: HashSet::with_capacity(n * 8),
            grounded_roots: HashSet::new(),
            grounded_ids: HashSet::new(),
        }
    }

    /// `tower_pre` is precomputed once by the caller and passed in unchanged
    /// every call — towers never move, so recomputing their trig every
    /// simulated second (as the old stateless version did) was pure waste.
    pub fn grounded_balloon_ids(
        &mut self,
        balloons: &[Balloon],
        towers: &[Tower],
        tower_pre: &[Precomputed],
        grid: &mut SpatialGrid,
        horizon_refraction_coeff: f64,
    ) -> &HashSet<u32> {
        let n_balloons = balloons.len();
        let n = n_balloons + towers.len();

        self.pre.clear();
        for (i, b) in balloons.iter().enumerate() {
            self.lons[i] = b.lon;
            self.lats[i] = b.lat;
            self.pre.push(precompute(b.lon, b.lat, b.alt, horizon_refraction_coeff));
        }
        for (i, t) in towers.iter().enumerate() {
            self.lons[n_balloons + i] = t.lon;
            self.lats[n_balloons + i] = t.lat;
        }
        self.pre.extend_from_slice(tower_pre);

        grid.soft_clear();
        for i in 0..n {
            grid.insert(i, self.lons[i], self.lats[i]);
        }

        let max_range_km = 2.0 * horizon_km(BALLOON_MAX_ALT, horizon_refraction_coeff);
        for (i, p) in self.parent.iter_mut().enumerate() {
            *p = i as u32;
        }

        // Pack the pair (i, j) into one u64 for the seen-pairs set — avoids
        // formatting/hashing strings for every candidate check, which is what
        // made the straightforward (String-keyed) port no faster than the JS
        // version despite being compiled/native.
        self.seen.clear();
        for i in 0..n {
            let candidates = grid.neighbors(self.lons[i], self.lats[i], max_range_km);
            for &j in &candidates {
                if j == i {
                    continue;
                }
                let (a, b) = if i < j { (i, j) } else { (j, i) };
                let key = ((a as u64) << 32) | (b as u64);
                if !self.seen.insert(key) {
                    continue;
                }
                if in_radio_range_precomputed(&self.pre[i], &self.pre[j]) {
                    union(&mut self.parent, i as u32, j as u32);
                }
            }
        }

        self.grounded_roots.clear();
        for ti in n_balloons..n {
            self.grounded_roots.insert(find(&mut self.parent, ti as u32));
        }
        self.grounded_ids.clear();
        for (bi, b) in balloons.iter().enumerate() {
            if self.grounded_roots.contains(&find(&mut self.parent, bi as u32)) {
                self.grounded_ids.insert(b.id);
            }
        }
        &self.grounded_ids
    }
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
        let grid_keys: HashSet<(NodeKey, NodeKey)> =
            grid_edges.iter().map(|e| ordered_pair(e.a, e.b)).collect();
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
        let grid_keys: HashSet<(NodeKey, NodeKey)> =
            grid_edges.iter().map(|e| ordered_pair(e.a, e.b)).collect();
        let ground_truth = brute_force_edge_keys(&balloons, &towers, horizon_coeff);
        let missed: Vec<_> = ground_truth.difference(&grid_keys).collect();
        assert!(missed.is_empty(), "reproduces the polar gap: {missed:?}");
    }
}
