// Ground-truth reachability (`grounded_pct`, from union-find over the
// balloons' physical adjacency) across the same (n, seed) grid as
// `density_sweep.rs`, so its chart can show the percolation ceiling
// alongside what each protocol actually delivers.
//
// Deliberately *not* part of `density_sweep.rs`: `grounded_pct` is computed
// purely from balloon positions and wind (see the union-find block in
// `World::tick`), before `self.protocol` is ever consulted — it is the same
// number for every protocol at a given (n, seed). Folding it into the
// per-protocol sweep would recompute an identical value 11 times; this
// binary runs it once.
//
//   cargo run --release --bin ground_truth_sweep [seeds] [rounds] > out.csv

use rayon::prelude::*;
use sim_server::config::{COMMS_EVERY_N_TICKS, INITIAL_TOWERS, TICK_DT_SECONDS, TIME_SCALE};
use sim_server::sim::World;
use sim_server::wind_field::WindField;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seeds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(35);
    let rounds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);

    // Same grid as density_sweep.rs.
    let ns: Vec<u32> = vec![100, 200, 400, 600, 800, 1200, 1600, 2000, 3000];

    println!("n,seed,meanDegree,groundedPct");

    let wind = Arc::new(WindField::zero());
    let mut jobs: Vec<(u32, u64)> = Vec::new();
    for &n in &ns {
        for seed in 0..seeds {
            jobs.push((n, seed));
        }
    }

    eprintln!(
        "{} runs ({} n values x {} seeds), rounds={rounds}, {} threads",
        jobs.len(),
        ns.len(),
        seeds,
        rayon::current_num_threads()
    );

    let rows: Vec<String> = jobs
        .par_iter()
        .map(|(n, seed)| {
            let mut world = World::new(Arc::clone(&wind)).with_seed(*seed);
            for &(lon, lat, h) in INITIAL_TOWERS {
                world.add_tower(lon, lat, h);
            }
            world.spawn_balloon_pool(*n);
            world.set_visible_count(*n);
            let dt = TICK_DT_SECONDS * TIME_SCALE;
            let mut last = world.tick(dt);
            for _ in 1..(rounds * COMMS_EVERY_N_TICKS) {
                last = world.tick(dt);
            }
            format!("{n},{seed},{},{}", last.mean_degree, last.grounded_pct)
        })
        .collect();

    for row in rows {
        println!("{row}");
    }
}
