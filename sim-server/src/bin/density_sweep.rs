// Per-seed CSV for all 11 `protocol_compare` variants, swept across balloon
// count as well as seed, so the density question `aggregation-summary.md`'s
// single-n snapshot (n=1200, 4 seeds) never asked can be answered: does
// `dv-dtn:ack=digest,mesh=4`'s lead over shipped dv-dtn (95.0 vs 72.3%
// completion there) hold as balloon density changes, or is it an artifact of
// that one density?
//
// Same reasoning as `discovery_sweep.rs` for going per-seed rather than
// `protocol_compare`'s mean+-sd: every protocol sees an identical balloon
// field at a given seed (the protocol RNG is independent of the world's), so
// paired per-seed rows let a plotting/summary script compare within a seed
// instead of fighting between-seed noise.
//
//   cargo run --release --bin density_sweep [seeds] [rounds] > out.csv
//
// Runs are independent (own World, own seed, sharing only a read-only zero
// wind field), so this is embarrassingly parallel via rayon, same as
// `discovery_sweep.rs`. It writes the CSV in one pass at the end rather than
// appending as rows finish, so an interrupted run is restarted rather than
// resumed — acceptable here because runs are cheap to redo individually via
// the pilot below, and it buys byte-identical output across thread counts.

use rayon::prelude::*;
use sim_server::config::{COMMS_EVERY_N_TICKS, INITIAL_TOWERS, TICK_DT_SECONDS, TIME_SCALE};
use sim_server::protocol::ProtocolSpec;
use sim_server::sim::World;
use sim_server::wind_field::WindField;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const KEYS: &[&str] = &[
    "originated",
    "delivered",
    "resolved",
    "unresolved",
    "unresolved_share",
    "completion_rate",
    "delivered_per_originated",
    "stall_rate",
    "delivered_per_slot_used",
    "ceiling_utilisation",
    "delivery_latency_mean",
    "delivery_latency_p95",
    "ack_latency_mean",
    "first_hop_latency_mean",
    "satellite",
    "dropped_loop",
    "dropped_ttl",
    "blocked",
    "stall_no_belief",
    "belief_hops_mean",
    "delivered_hops_mean",
    "gossip_redundant_share",
    "mpr_share",
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seeds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(80);
    let rounds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);

    // All 11 `protocol_compare` variants (sim-server/src/bin/protocol_compare.rs:127-152).
    let specs: Vec<(&str, &str)> = vec![
        ("dv-dtn", "dv-dtn"),
        ("digest-mesh4", "dv-dtn:ack=digest,mesh=4"),
        ("reactive", "dv-dtn:discovery=reactive"),
        ("reactive-tower", "dv-dtn:discovery=reactive,reply=tower"),
        ("reactive-overhear", "dv-dtn:discovery=reactive,overhear=on"),
        ("reactive-ring", "dv-dtn:discovery=reactive,ring=expanding"),
        (
            "reactive-overhear-ring",
            "dv-dtn:discovery=reactive,overhear=on,ring=expanding",
        ),
        ("linkstate", "dv-dtn:discovery=link-state"),
        ("linkstate-mpr", "dv-dtn:discovery=link-state,relay=mpr"),
        ("spray-l4", "epidemic:copies=4"),
        ("spray-l16", "epidemic:copies=16"),
    ];

    let ns: Vec<u32> = vec![100, 200, 400, 600, 800, 1200, 1600, 2000, 3000];

    print!("protocol,n,seed");
    for k in KEYS {
        print!(",{k}");
    }
    println!();

    let wind = Arc::new(WindField::zero());
    let mut jobs: Vec<(&str, &str, u32, u64)> = Vec::new();
    for (label, spec) in &specs {
        for seed in 0..seeds {
            for &n in &ns {
                jobs.push((*label, *spec, n, seed));
            }
        }
    }

    eprintln!(
        "{} runs ({} variants x {} n values x {} seeds), rounds={rounds}, {} threads",
        jobs.len(),
        specs.len(),
        ns.len(),
        seeds,
        rayon::current_num_threads()
    );
    let done = AtomicUsize::new(0);
    let total = jobs.len();

    let rows: Vec<String> = jobs
        .par_iter()
        .map(|(label, spec_str, n, seed)| {
            let spec: ProtocolSpec = spec_str.parse().expect("spec should parse");
            let mut world = World::new(Arc::clone(&wind)).with_protocol(spec).with_seed(*seed);
            for &(lon, lat, h) in INITIAL_TOWERS {
                world.add_tower(lon, lat, h);
            }
            world.spawn_balloon_pool(*n);
            world.set_visible_count(*n);
            let dt = TICK_DT_SECONDS * TIME_SCALE;
            for _ in 0..rounds {
                for _ in 0..COMMS_EVERY_N_TICKS {
                    world.tick(dt);
                }
            }
            let table = world.stats();

            let mut row = format!("{label},{n},{seed}");
            for k in KEYS {
                match table.get(k) {
                    Some(v) => row.push_str(&format!(",{v}")),
                    None => row.push(','),
                }
            }
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if d.is_multiple_of(50) || d == total {
                eprintln!("{d}/{total}");
            }
            row
        })
        .collect();

    for row in rows {
        println!("{row}");
    }
}
