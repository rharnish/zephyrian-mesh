// Omniscient measurement probe (offline, no wind fetch, no server).
//
// Answers the question that sets the beacon parameters for the mesh-comms
// design: *how many hops is a typical balloon from the nearest tower?* A
// duty-cycled beacon advances one hop per beacon interval, so hop depth is
// the multiplier that turns "beacon every T minutes" into "belief lags truth
// by N*T minutes". Also reports node degree (how dense the mesh is) and the
// unreachable fraction (balloons no beacon can ever reach).
//
// Static snapshot only: positions are sampled uniformly on the sphere, the
// same way spawn_balloon_pool does, and edges come from the real
// link_detection code. Wind is irrelevant here because we're measuring the
// spatial statistics of the graph, not its evolution.
//
//   cargo run --release --bin mesh_depth

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use sim_server::balloon::Balloon;
use sim_server::config::{
    BALLOON_MAX_ALT, BALLOON_MIN_ALT, DEFAULT_HORIZON_REFRACTION_COEFF, GRID_CELL_SIZE_DEG,
    INITIAL_TOWERS,
};
use sim_server::geo::{horizon_km, random_global_position};
use sim_server::link_detection::{compute_grid_edges, NodeKey};
use sim_server::spatial_grid::SpatialGrid;
use sim_server::tower::Tower;
use std::collections::{HashMap, VecDeque};

const SEEDS: u64 = 8;
const COUNTS: &[u32] = &[200, 400, 800, 1200, 2000];
// The frontend slider (src/main.js) allows 2.5..4.2 — 4.12 is the default and
// near the optimistic end, so sweep the whole usable range. 3.57 is pure
// geometric line-of-sight with no atmospheric refraction.
const COEFFS: &[f64] = &[2.5, 3.0, 3.57, 4.12];

/// Multi-source BFS from every tower. Returns hop depth per balloon key,
/// where depth 1 == balloon talks directly to a tower.
fn depths_from_towers(
    adj: &HashMap<NodeKey, Vec<NodeKey>>,
    towers: &[Tower],
) -> HashMap<NodeKey, u32> {
    let mut depth: HashMap<NodeKey, u32> = HashMap::new();
    let mut q: VecDeque<NodeKey> = VecDeque::new();
    for t in towers {
        let key = NodeKey::Tower(t.id);
        depth.insert(key, 0);
        q.push_back(key);
    }
    while let Some(cur) = q.pop_front() {
        let d = depth[&cur];
        if let Some(nbrs) = adj.get(&cur) {
            for &n in nbrs {
                if !depth.contains_key(&n) {
                    depth.insert(n, d + 1);
                    q.push_back(n);
                }
            }
        }
    }
    depth
}

fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

fn main() {
    println!(
        "{} towers, {} seeds per cell. Balloon altitudes uniform in [{:.0}, {:.0}] m, so the \
         *typical* balloon horizon is well below the max quoted per block.\n",
        INITIAL_TOWERS.len(),
        SEEDS,
        BALLOON_MIN_ALT,
        BALLOON_MAX_ALT,
    );

    for &coeff in COEFFS {
        let max_range_km = 2.0 * horizon_km(BALLOON_MAX_ALT, coeff);
        let mid_alt = 0.5 * (BALLOON_MIN_ALT + BALLOON_MAX_ALT);
        println!(
            "=== horizon coeff {coeff}{}  |  typical pair range {:.0} km (mid-alt {:.0} m)  |  \
             max pair range {:.0} km  |  tower@30m {:.0} km",
            if (coeff - DEFAULT_HORIZON_REFRACTION_COEFF).abs() < 1e-9 { " [DEFAULT]" } else { "" },
            2.0 * horizon_km(mid_alt, coeff),
            mid_alt,
            max_range_km,
            horizon_km(30.0, coeff),
        );
        println!(
            "{:>6}  {:>9}  {:>7}  {:>7}  {:>6}  {:>6}  {:>6}  {:>6}",
            "n", "unreach%", "avg deg", "isolated", "med", "p90", "p95", "max"
        );
        run_counts(coeff, max_range_km);
        println!();
    }

    // Sanity: how far does a balloon actually drift between link rounds?
    // LINK_UPDATE_EVERY_N_TICKS * TICK_DT * TIME_SCALE seconds of wind.
    let round_sec = sim_server::config::LINK_UPDATE_EVERY_N_TICKS as f64
        * sim_server::config::TICK_DT_SECONDS
        * sim_server::config::TIME_SCALE;
    println!(
        "link round = {:.0} sim seconds ({:.1} sim min). At 30 m/s a balloon moves {:.1} km/round.",
        round_sec,
        round_sec / 60.0,
        30.0 * round_sec / 1000.0,
    );
}

fn run_counts(coeff: f64, max_range_km: f64) {
    for &n in COUNTS {
        let mut unreach_pct_sum = 0.0;
        let mut deg_sum = 0.0;
        let mut isolated_pct_sum = 0.0;
        let mut med_sum = 0.0;
        let mut p90_sum = 0.0;
        let mut p95_sum = 0.0;
        let mut max_sum = 0.0;
        let mut hist: HashMap<u32, u64> = HashMap::new();

        for seed in 0..SEEDS {
            let mut rng = StdRng::seed_from_u64(seed);
            let towers: Vec<Tower> = INITIAL_TOWERS
                .iter()
                .enumerate()
                .map(|(i, &(lon, lat, h))| Tower::new(i as u32, lon, lat, h))
                .collect();
            let balloons: Vec<Balloon> = (0..n)
                .map(|id| {
                    let (lon, lat) = random_global_position(&mut rng);
                    let alt =
                        BALLOON_MIN_ALT + rng.gen_range(0.0..(BALLOON_MAX_ALT - BALLOON_MIN_ALT));
                    Balloon::new(id, lon, lat, alt)
                })
                .collect();

            let mut grid = SpatialGrid::new(GRID_CELL_SIZE_DEG);
            let edges = compute_grid_edges(&balloons, &towers, &mut grid, max_range_km, coeff);

            let mut adj: HashMap<NodeKey, Vec<NodeKey>> = HashMap::new();
            for e in &edges {
                adj.entry(e.a).or_default().push(e.b);
                adj.entry(e.b).or_default().push(e.a);
            }

            let depth = depths_from_towers(&adj, &towers);

            let mut reached: Vec<u32> = Vec::new();
            let mut unreachable = 0u64;
            let mut isolated = 0u64;
            let mut deg_total = 0u64;
            for b in &balloons {
                let key = NodeKey::Balloon(b.id);
                let d = adj.get(&key).map(|v| v.len()).unwrap_or(0);
                deg_total += d as u64;
                if d == 0 {
                    isolated += 1;
                }
                match depth.get(&key) {
                    Some(&dep) => reached.push(dep),
                    None => unreachable += 1,
                }
            }
            reached.sort_unstable();
            for &d in &reached {
                *hist.entry(d).or_default() += 1;
            }

            unreach_pct_sum += 100.0 * unreachable as f64 / n as f64;
            isolated_pct_sum += 100.0 * isolated as f64 / n as f64;
            deg_sum += deg_total as f64 / n as f64;
            med_sum += percentile(&reached, 0.50) as f64;
            p90_sum += percentile(&reached, 0.90) as f64;
            p95_sum += percentile(&reached, 0.95) as f64;
            max_sum += reached.last().copied().unwrap_or(0) as f64;
        }

        let s = SEEDS as f64;
        println!(
            "{:>6}  {:>8.1}%  {:>7.1}  {:>6.1}%  {:>6.1}  {:>6.1}  {:>6.1}  {:>6.1}",
            n,
            unreach_pct_sum / s,
            deg_sum / s,
            isolated_pct_sum / s,
            med_sum / s,
            p90_sum / s,
            p95_sum / s,
            max_sum / s,
        );

        let _ = &hist;
    }
}
