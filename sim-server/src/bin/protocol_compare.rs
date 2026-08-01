// Runs several protocols over the same balloon fields and prints their
// counters side by side.
//
// This is the first harness that does not know which protocol it is driving.
// Everything under bin/ up to now reached for `world.bundle_stats()`, which is
// dv-dtn's concrete type and panics under anything else — fine while there was
// one protocol, useless the moment there were two. It reads `world.stats()`
// instead, a keyed table each protocol fills in with whatever it actually
// counts.
//
// Only four keys are expected to mean the same thing everywhere — originated,
// delivered, resolved, completion_rate — and those are what the comparison
// table uses. Everything else is printed per protocol, unaligned, because
// aligning `stall_no_belief` against `duplicate_arrivals` would be inventing a
// correspondence that isn't there.
//
//   cargo run --release --bin protocol_compare [seeds] [n] [rounds]
//
// Fields are shared: at a given seed every protocol sees an identical balloon
// field, because the protocol's RNG is independent of the world's. Differences
// between rows are the protocol, not the draw.

use sim_server::config::{
    COMMS_EVERY_N_TICKS, DEFAULT_HORIZON_REFRACTION_COEFF, INITIAL_TOWERS, TICK_DT_SECONDS,
    TIME_SCALE,
};
use sim_server::protocol::stats::StatsTable;
use sim_server::protocol::ProtocolSpec;
use sim_server::sim::World;
use sim_server::wind_field::WindField;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Keys every delivery protocol reports, and which therefore compare.
const CANONICAL: &[&str] = &["originated", "delivered", "resolved", "completion_rate"];

fn run(spec: &ProtocolSpec, seed: u64, n: u32, rounds: u64) -> StatsTable {
    let mut world = World::new(Arc::new(WindField::zero()))
        .with_protocol(spec.clone())
        .with_seed(seed);
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
    world.stats()
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

fn sd(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let m = mean(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seeds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
    let n: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1200);
    let rounds: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(600);

    let specs: Vec<(&str, ProtocolSpec)> = vec![
        ("dv-dtn (shipped)", "dv-dtn".parse().unwrap()),
        ("dv-dtn digest+batch", "dv-dtn:ack=digest,mesh=4".parse().unwrap()),
        ("dv-dtn reactive (AODV)", "dv-dtn:discovery=reactive".parse().unwrap()),
        ("spray-and-wait L=4", "epidemic:copies=4".parse().unwrap()),
        ("spray-and-wait L=16", "epidemic:copies=16".parse().unwrap()),
    ];

    println!("n={n}  rounds={rounds}  seeds={seeds}  zero wind  coeff={DEFAULT_HORIZON_REFRACTION_COEFF}");
    println!("(identical balloon field per seed across every protocol)\n");

    // key -> label -> per-seed values
    let mut all: BTreeMap<&'static str, BTreeMap<&str, Vec<f64>>> = BTreeMap::new();
    for (label, spec) in &specs {
        for seed in 0..seeds {
            let table = run(spec, seed, n, rounds);
            for (k, v) in table.iter() {
                all.entry(k).or_default().entry(label).or_default().push(v.as_f64());
            }
        }
    }

    println!("## Comparable across protocols\n");
    let w = specs.iter().map(|(l, _)| l.len()).max().unwrap_or(20);
    print!("{:<w$}", "protocol", w = w);
    for k in CANONICAL {
        print!("  {:>16}", k);
    }
    println!();
    println!("{}", "-".repeat(w + CANONICAL.len() * 18));
    for (label, _) in &specs {
        print!("{:<w$}", label, w = w);
        for k in CANONICAL {
            let v = all.get(*k).and_then(|m| m.get(label)).cloned().unwrap_or_default();
            let cell = if *k == "completion_rate" {
                format!("{:.1}±{:.1}%", mean(&v) * 100.0, sd(&v) * 100.0)
            } else {
                format!("{:.0}", mean(&v))
            };
            print!("  {cell:>16}");
        }
        println!();
    }

    println!("\n## Protocol-specific\n");
    for (label, _) in &specs {
        println!("{label}:");
        for (k, per) in &all {
            if CANONICAL.contains(k) {
                continue;
            }
            if let Some(v) = per.get(label) {
                println!("    {:<28} {:>12.2}", k, mean(v));
            }
        }
        println!();
    }
}
