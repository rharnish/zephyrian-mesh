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
//   completion_rate      = delivered / resolved     (of bundles that finished)
//   delivered_per_orig   = delivered / originated   (of bundles that started)
//
// A protocol that leaves bundles stalled in queues at the end of the run never
// resolves them, so they leave the first denominator and not the second.
// Reactive discovery strands a lot of bundles, so the two ratios rank it
// differently — reporting only the flattering one would be a choice, not a
// measurement.
//
//   cargo run --release --bin discovery_sweep [seeds] [n] [rounds] > out.csv

use sim_server::config::{
    COMMS_EVERY_N_TICKS, INITIAL_TOWERS, TICK_DT_SECONDS, TIME_SCALE,
};
use sim_server::protocol::ProtocolSpec;
use sim_server::sim::World;
use sim_server::wind_field::WindField;
use std::sync::Arc;

/// Counters worth carrying through to the summary. Kept explicit rather than
/// dumping the whole table: these are the ones that say *why* a variant moved,
/// and a CSV nobody can read is not evidence.
const KEYS: &[&str] = &[
    "originated",
    "delivered",
    "resolved",
    "completion_rate",
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
    println!(",delivered_per_orig");

    let wind = Arc::new(WindField::zero());
    for (label, spec_str) in &specs {
        let spec: ProtocolSpec = spec_str.parse().expect("spec should parse");
        for seed in 0..seeds {
            let mut world =
                World::new(Arc::clone(&wind)).with_protocol(spec.clone()).with_seed(seed);
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

            print!("{label},{seed}");
            for k in KEYS {
                match table.get(k) {
                    Some(v) => print!(",{v}"),
                    None => print!(","), // not a counter this variant keeps
                }
            }
            let orig = table.get("originated").unwrap_or(0.0);
            let del = table.get("delivered").unwrap_or(0.0);
            println!(",{}", if orig == 0.0 { 0.0 } else { del / orig });
        }
        eprintln!("done {label}");
    }
}
