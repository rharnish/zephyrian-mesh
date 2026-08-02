// Per-seed CSV for the discovery variants, so their differences can be read
// *paired* rather than as overlapping error bars.
//
// Why this exists rather than more seeds in `protocol_compare`: that harness
// prints mean +/- sd across seeds, and the between-seed spread here is 6-8
// completion points while the effects under test are 1-3. Reading those means
// side by side cannot separate "no effect" from "an effect the seed noise
// buries" — the same trap the wind study fell into at 4 seeds and escaped only
// by comparing within each seed. Every protocol sees an identical balloon field
// at a given seed (the protocol RNG is independent of the world's), so the
// per-seed difference is the protocol and nothing else.
//
// It also emits **both** delivery ratios, because they disagree here and the
// disagreement is informative:
//
//   completion_rate           = delivered / resolved   (of bundles that finished)
//   delivered_per_originated  = delivered / originated (of bundles that started)
//   unresolved_share          = the gap between them, made explicit
//
// A protocol that leaves bundles stalled in queues at the end of the run never
// resolves them, so they leave the first denominator and not the second.
// Reactive discovery strands a lot of bundles, so the two ratios rank it
// differently — reporting only the flattering one would be a choice, not a
// measurement.
//
//   cargo run --release --bin discovery_sweep [seeds] [n] [rounds] > out.csv
//   RAYON_NUM_THREADS=2 ./target/release/discovery_sweep 20 1200 400 > out.csv
//
// Runs are independent — own `World`, own seed, sharing only a read-only zero
// wind field — so this is embarrassingly parallel and uses every core by
// default. Unlike `aggregation_sweep` and `wind_sweep` it writes the CSV in one
// pass at the end rather than appending as rows finish, so it does **not**
// resume: an interrupted run is restarted. That buys byte-identical output
// across thread counts, which those two give up in exchange for resumability.
// At ~10 minutes on four cores it is the better trade here.

use rayon::prelude::*;
use sim_server::config::{
    COMMS_EVERY_N_TICKS, INITIAL_TOWERS, TICK_DT_SECONDS, TIME_SCALE,
};
use sim_server::protocol::ProtocolSpec;
use sim_server::sim::World;
use sim_server::wind_field::WindField;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Counters worth carrying through to the summary. Kept explicit rather than
/// dumping the whole table: these are the ones that say *why* a variant moved,
/// and a CSV nobody can read is not evidence.
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
    let seeds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let n: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1200);
    let rounds: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(400);

    let specs: Vec<(&str, &str)> = vec![
        ("proactive", "dv-dtn"),
        // The aggregation levers are in this sweep purely for their *latency*.
        // Their delivery effect was settled at 24 seeds in aggregation-summary
        // and is not re-litigated here; the open question is whether batching
        // buys those points with time nobody was measuring.
        ("proactive+digest", "dv-dtn:ack=digest"),
        ("proactive+mesh4", "dv-dtn:mesh=4"),
        ("proactive+digest+mesh4", "dv-dtn:ack=digest,mesh=4"),
        ("proactive+mesh8", "dv-dtn:mesh=8"),
        ("reactive", "dv-dtn:discovery=reactive"),
        ("reactive+overhear", "dv-dtn:discovery=reactive,overhear=on"),
        ("reactive+ring", "dv-dtn:discovery=reactive,ring=expanding"),
        ("reactive+overhear+ring", "dv-dtn:discovery=reactive,overhear=on,ring=expanding"),
        ("linkstate-lsa2", "dv-dtn:discovery=link-state,lsa=2"),
        ("linkstate-lsa2-mpr", "dv-dtn:discovery=link-state,lsa=2,relay=mpr"),
        ("linkstate", "dv-dtn:discovery=link-state"),
        ("linkstate-mpr", "dv-dtn:discovery=link-state,relay=mpr"),
        ("linkstate-lsa16", "dv-dtn:discovery=link-state,lsa=16"),
        ("linkstate-lsa16-mpr", "dv-dtn:discovery=link-state,lsa=16,relay=mpr"),
    ];

    print!("protocol,seed");
    for k in KEYS {
        print!(",{k}");
    }
    println!();

    // One job per (variant, seed). Every run builds its own `World` and shares
    // nothing but a read-only zero wind field, so this parallelises with no
    // coordination at all — the protocol keeps no global state (the last piece,
    // ablation.rs's process-wide AtomicBool, was deleted with the trait seam).
    let wind = Arc::new(WindField::zero());
    let jobs: Vec<(&str, &str, u64)> = specs
        .iter()
        .flat_map(|(label, spec)| (0..seeds).map(move |s| (*label, *spec, s)))
        .collect();

    eprintln!(
        "{} runs ({} variants x {} seeds), n={n}, {} rounds, {} threads",
        jobs.len(),
        specs.len(),
        seeds,
        rounds,
        rayon::current_num_threads()
    );
    let done = AtomicUsize::new(0);
    let total = jobs.len();

    // `Vec::par_iter().map(..).collect()` is an *indexed* parallel iterator, so
    // rows come back in job order however the threads interleave. That matters
    // more than it sounds: it keeps the CSV byte-comparable between runs and
    // between thread counts, so `RAYON_NUM_THREADS=1` and the default produce
    // diffable output rather than the same rows shuffled.
    let rows: Vec<String> = jobs
        .par_iter()
        .map(|(label, spec_str, seed)| {
            let spec: ProtocolSpec = spec_str.parse().expect("spec should parse");
            let mut world =
                World::new(Arc::clone(&wind)).with_protocol(spec).with_seed(*seed);
            for &(lon, lat, h) in INITIAL_TOWERS {
                world.add_tower(lon, lat, h);
            }
            world.spawn_balloon_pool(n);
            world.set_visible_count(n);
            let dt = TICK_DT_SECONDS * TIME_SCALE;
            for _ in 0..rounds {
                for _ in 0..COMMS_EVERY_N_TICKS {
                    world.tick(dt);
                }
            }
            let table = world.stats();

            let mut row = format!("{label},{seed}");
            for k in KEYS {
                match table.get(k) {
                    Some(v) => row.push_str(&format!(",{v}")),
                    None => row.push(','), // not a counter this variant keeps
                }
            }
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if d.is_multiple_of(10) || d == total {
                eprintln!("{d}/{total}");
            }
            row
        })
        .collect();

    for row in rows {
        println!("{row}");
    }
}
