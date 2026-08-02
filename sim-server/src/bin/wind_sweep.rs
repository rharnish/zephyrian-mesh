// Every protocol, over many seeds, over several real weather fields.
//
// The experiment this exists for: `protocol_compare --wind` found that real
// wind moves nothing — dv-dtn 72.3% -> 72.9%, spray-and-wait 23.5% -> 23.7% —
// and concluded that store-carry-forward already absorbs churn, so replication
// cannot close the gap by being robust to it. That conclusion rests on **one**
// ERA5 time step, four seeds, and columns that are not paired (balloons
// diverge once wind is applied, so the two conditions no longer share a
// field). It is the right shape of claim on thin evidence.
//
// This crosses wind with seed so the claim can be made properly:
//
//   - every (wind, seed) cell runs every protocol, so protocols are compared
//     *within* a cell, where the balloon field is identical by construction
//     (MeshProtocol::reseed keeps the protocol's RNG off the world's stream)
//   - weather becomes a blocking factor rather than a fixed constant, so
//     "wind changes nothing" can be a statement about weather in general
//     rather than about one afternoon
//   - zero wind is one of the levels, so the frozen-topology baseline every
//     earlier result was measured on stays in the same table
//
//   cargo run --release --bin wind_sweep [seeds] [rounds]
//   RAYON_NUM_THREADS=2 nohup ./target/release/wind_sweep 12 400 &
//
// Resumable exactly like aggregation_sweep: finished rows are appended
// immediately and re-read on startup, so this can be killed, restarted, or
// extended with more seeds later.
//
// **Wind is the outer loop on purpose.** Each field is ~150MB and rayon
// parallelises over the seeds *within* one field, so only one is ever
// resident. Iterating protocols or seeds outermost would hold all of them.

use rayon::prelude::*;
use sim_server::config::{
    COMMS_EVERY_N_TICKS, INITIAL_TOWERS, TICK_DT_SECONDS, TIME_SCALE,
};
use sim_server::protocol::ProtocolSpec;
use sim_server::sim::World;
use sim_server::wind_cache::{self, WindCache};
use sim_server::wind_field::WindField;
use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

const OUT_CSV: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../experiments/wind-sweep-results.csv");
const HEADER: &str = "wind,seed,protocol,nBalloons,rounds,meanDegree,groundedPct,\
believedGroundedPct,originated,delivered,resolved,completionRate,satellite,acked,ackLost,\
droppedLoop,droppedTtl,blocked,stallNoBelief,stallStaleNextHop,beliefHopsMean,deliveredHopsMean";

/// The protocols compared. Kept to the ones with something at stake in the
/// churn question: the shipped baseline, its best-known configuration, the two
/// alternative discovery modes, and replication at both copy budgets.
const SPECS: &[(&str, &str)] = &[
    ("dv-dtn", "dv-dtn"),
    ("dv-dtn-digest-batch", "dv-dtn:ack=digest,mesh=4"),
    ("dv-dtn-reactive", "dv-dtn:discovery=reactive"),
    ("dv-dtn-link-state", "dv-dtn:discovery=link-state"),
    ("spray-4", "epidemic:copies=4"),
    ("spray-16", "epidemic:copies=16"),
];

const N_BALLOONS: u32 = 1200;

fn row_key(wind: &str, seed: u64, protocol: &str) -> String {
    format!("{wind},{seed},{protocol}")
}

fn get(t: &sim_server::protocol::stats::StatsTable, k: &str) -> f64 {
    t.get(k).unwrap_or(f64::NAN)
}

fn run_one(
    spec: &ProtocolSpec,
    seed: u64,
    rounds: u64,
    wind: &Arc<WindField>,
) -> (sim_server::protocol::stats::StatsTable, f64, f64, f64) {
    let mut world =
        World::new(Arc::clone(wind)).with_protocol(spec.clone()).with_seed(seed);
    for &(lon, lat, h) in INITIAL_TOWERS {
        world.add_tower(lon, lat, h);
    }
    world.spawn_balloon_pool(N_BALLOONS);
    world.set_visible_count(N_BALLOONS);

    let dt = TICK_DT_SECONDS * TIME_SCALE;
    let mut last = None;
    for _ in 0..rounds {
        for _ in 0..COMMS_EVERY_N_TICKS {
            last = Some(world.tick(dt));
        }
    }
    let s = last.expect("at least one tick");
    (world.stats(), s.mean_degree, s.grounded_pct, s.believed_grounded_pct)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seeds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(12);
    let rounds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);

    // Zero wind first: it is the control, and it is also the cheapest, so a
    // run killed early still has the baseline column complete.
    let mut winds: Vec<String> = vec!["none".to_string()];
    match WindCache::open().and_then(|c| c.entries()) {
        Ok(entries) => winds.extend(entries.iter().map(|e| e.label.clone())),
        Err(e) => {
            eprintln!("error reading wind cache: {e}");
            std::process::exit(1);
        }
    }
    if winds.len() == 1 {
        eprintln!(
            "wind cache is empty — this sweep would just re-measure zero wind.\n\
             Populate it first: cargo run --release --bin wind_cache -- fetch 0 4 8 12 16 20"
        );
        std::process::exit(1);
    }

    let specs: Vec<(&str, ProtocolSpec)> =
        SPECS.iter().map(|(name, s)| (*name, s.parse().expect("built-in spec must parse"))).collect();

    let done: HashSet<String> = std::fs::read_to_string(OUT_CSV)
        .map(|s| {
            s.lines()
                .skip(1)
                .filter_map(|l| {
                    let f: Vec<&str> = l.split(',').collect();
                    (f.len() > 3).then(|| f[..3].join(","))
                })
                .collect()
        })
        .unwrap_or_default();

    if !std::path::Path::new(OUT_CSV).exists() {
        std::fs::write(OUT_CSV, format!("{HEADER}\n")).expect("write header");
    }

    let total = winds.len() * seeds as usize * specs.len();
    let mut remaining = 0usize;
    for w in &winds {
        for s in 0..seeds {
            for (name, _) in &specs {
                if !done.contains(&row_key(w, s, name)) {
                    remaining += 1;
                }
            }
        }
    }
    eprintln!(
        "{} winds x {} seeds x {} protocols = {} rows; {} already done, {} to run\n\
         ({} rounds each, n={}, {} threads)",
        winds.len(),
        seeds,
        specs.len(),
        total,
        total - remaining,
        remaining,
        rounds,
        N_BALLOONS,
        rayon::current_num_threads(),
    );

    let file = Mutex::new(
        std::fs::OpenOptions::new().append(true).open(OUT_CSV).expect("open csv for append"),
    );
    let progress = Mutex::new(0usize);

    for wind_name in &winds {
        // One field resident at a time; see the header note.
        let field = match wind_cache::resolve(wind_name) {
            Ok(f) => Arc::new(f),
            Err(e) => {
                eprintln!("skipping wind {wind_name:?}: {e}");
                continue;
            }
        };
        eprintln!("--- wind: {wind_name} ---");

        let jobs: Vec<(u64, &(&str, ProtocolSpec))> = (0..seeds)
            .flat_map(|s| specs.iter().map(move |sp| (s, sp)))
            .filter(|(s, (name, _))| !done.contains(&row_key(wind_name, *s, name)))
            .collect();

        jobs.par_iter().for_each(|(seed, (pname, spec))| {
            let (t, degree, grounded, believed) = run_one(spec, *seed, rounds, &field);
            let line = format!(
                "{},{},{},{},{},{:.4},{:.2},{:.2},{},{},{},{:.4},{},{},{},{},{},{},{},{},{:.3},{:.3}",
                wind_name,
                seed,
                pname,
                N_BALLOONS,
                rounds,
                degree,
                grounded,
                believed,
                get(&t, "originated"),
                get(&t, "delivered"),
                get(&t, "resolved"),
                get(&t, "completion_rate"),
                get(&t, "satellite"),
                get(&t, "acked"),
                get(&t, "ack_lost"),
                get(&t, "dropped_loop"),
                get(&t, "dropped_ttl"),
                get(&t, "blocked"),
                get(&t, "stall_no_belief"),
                get(&t, "stall_stale_next_hop"),
                get(&t, "belief_hops_mean"),
                get(&t, "delivered_hops_mean"),
            );
            {
                let mut f = file.lock().unwrap();
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
            let mut p = progress.lock().unwrap();
            *p += 1;
            eprintln!("[{}/{}] {wind_name} seed={seed} {pname}", *p, remaining);
        });
    }
    eprintln!("done -> {OUT_CSV}");
}
