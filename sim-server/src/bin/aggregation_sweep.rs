// The aggregation axis, measured properly: many seeds, three densities, both
// ack policies, the full batch grid.
//
// Why this exists rather than bin/batch_sweep.rs, which asks the same
// question: single-seed results in this simulation are dominated by how many
// balloons happen to sit in tower range, which varies a lot run to run and
// correlates with delivered/round at r = 0.94 (see DvDtnParams::relay_queue
// docs and MESH_COMMS_DESIGN.md §4). A single seed can resolve "1 is clearly
// too small" and essentially nothing finer. batch_sweep is the quick look;
// this is the one whose numbers can be quoted.
//
// Comparisons across rows *at one seed* are clean by construction: the
// protocol draws from its own RNG stream, so changing any parameter here
// leaves the balloon field identical (see MeshProtocol::reseed and the
// protocol_choice_does_not_perturb_the_balloon_field test). Seeds vary the
// field on purpose, to get an error bar on top of that.
//
//   cargo run --release --bin aggregation_sweep [seeds] [rounds]
//   RAYON_NUM_THREADS=2 cargo run --release --bin aggregation_sweep   # be nice
//
// Resumable: every finished row is appended to the CSV immediately and
// re-read on startup, so killing this and restarting picks up where it left
// off. That matters — a full grid is hours.

use rayon::prelude::*;
use sim_server::config::{
    COMMS_EVERY_N_TICKS, DEFAULT_HORIZON_REFRACTION_COEFF, INITIAL_TOWERS, TICK_DT_SECONDS,
    TIME_SCALE,
};
use sim_server::protocol::dv_dtn::params::{AckPolicy, BatchPolicy, DvDtnParams};
use sim_server::protocol::ProtocolSpec;
use sim_server::sim::World;
use sim_server::wind_field::WindField;
use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

// Resolved against the crate, not the cwd — this gets launched from both
// the repo root and sim-server/.
const OUT_CSV: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../experiments/aggregation-sweep-results.csv");
const HEADER: &str = "seed,nBalloons,ackPolicy,towerContact,meshHop,rounds,meanDegree,groundedPct,\
believedGroundedPct,originated,delivered,satellite,acked,ackLost,droppedLoop,droppedTtl,blocked,\
completionRate,meanTowerAdjacent,deliveryCeilingPerRound,slotsWithBundle,stallNoBelief,\
stallStaleNextHop";

#[derive(Clone, Copy, PartialEq)]
struct Combo {
    seed: u64,
    n: u32,
    ack: AckPolicy,
    tower_contact: usize,
    mesh_hop: usize,
}

fn ack_name(a: AckPolicy) -> &'static str {
    match a {
        AckPolicy::SourceRouted => "source-routed",
        AckPolicy::Digest => "digest",
    }
}

impl Combo {
    /// Identity as it appears in the CSV, for the resume check.
    fn key(&self) -> String {
        format!(
            "{},{},{},{},{}",
            self.seed,
            self.n,
            ack_name(self.ack),
            self.tower_contact,
            self.mesh_hop
        )
    }
}

fn advance_round(world: &mut World) -> sim_server::sim::Snapshot {
    let dt = TICK_DT_SECONDS * TIME_SCALE;
    let mut s = world.tick(dt);
    for _ in 1..COMMS_EVERY_N_TICKS {
        s = world.tick(dt);
    }
    s
}

fn run(combo: Combo, rounds: u64) -> String {
    let params = DvDtnParams {
        batch: BatchPolicy { mesh_hop: combo.mesh_hop, tower_contact: combo.tower_contact },
        ack_policy: combo.ack,
        ..Default::default()
    };
    let mut world = World::new(Arc::new(WindField::zero()))
        .with_protocol(ProtocolSpec::DvDtn(params))
        .with_seed(combo.seed);
    for &(lon, lat, h) in INITIAL_TOWERS {
        world.add_tower(lon, lat, h);
    }
    world.spawn_balloon_pool(combo.n);
    world.set_visible_count(combo.n);

    let mut last = None;
    for _ in 0..rounds {
        last = Some(advance_round(&mut world));
    }
    let s = last.expect("at least one round");
    let st = world.bundle_stats();
    format!(
        "{},{},{:.4},{:.4},{:.4},{},{},{},{},{},{},{},{},{:.6},{:.4},{:.4},{},{},{}",
        combo.key(),
        rounds,
        s.mean_degree,
        s.grounded_pct,
        s.believed_grounded_pct,
        st.originated,
        st.delivered,
        st.satellite,
        st.acked,
        st.ack_lost,
        st.dropped_loop,
        st.dropped_ttl,
        st.blocked,
        st.completion_rate(),
        st.mean_tower_adjacent(),
        st.delivery_capacity_per_round(&params),
        st.slots_with_bundle,
        st.stall_no_belief,
        st.stall_stale_next_hop,
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seeds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let rounds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1200);

    let mut combos = Vec::new();
    for seed in 0..seeds {
        for &n in &[600u32, 1200, 2000] {
            for &ack in &[AckPolicy::SourceRouted, AckPolicy::Digest] {
                for &tower_contact in &[1usize, 4] {
                    for &mesh_hop in &[1usize, 2, 4, 8] {
                        combos.push(Combo { seed, n, ack, tower_contact, mesh_hop });
                    }
                }
            }
        }
    }

    // Resume: anything already in the CSV is skipped, so this can be killed
    // and restarted, or extended with more seeds later.
    let done: HashSet<String> = std::fs::read_to_string(OUT_CSV)
        .map(|s| {
            s.lines()
                .skip(1)
                .filter_map(|l| {
                    let f: Vec<&str> = l.split(',').collect();
                    (f.len() > 5).then(|| f[..5].join(","))
                })
                .collect()
        })
        .unwrap_or_default();
    let todo: Vec<Combo> = combos.iter().copied().filter(|c| !done.contains(&c.key())).collect();

    if !std::path::Path::new(OUT_CSV).exists() {
        std::fs::write(OUT_CSV, format!("{HEADER}\n")).expect("write header");
    }
    eprintln!(
        "{} combos total, {} already done, {} to run ({} rounds each, {} threads)",
        combos.len(),
        combos.len() - todo.len(),
        todo.len(),
        rounds,
        rayon::current_num_threads(),
    );

    let file = Mutex::new(
        std::fs::OpenOptions::new().append(true).open(OUT_CSV).expect("open csv for append"),
    );
    let progress = std::sync::atomic::AtomicUsize::new(0);
    let started = std::time::Instant::now();
    let total = todo.len();

    todo.par_iter().for_each(|&combo| {
        let row = run(combo, rounds);
        let n = progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        {
            let mut f = file.lock().expect("csv lock");
            writeln!(f, "{row}").expect("append row");
            f.flush().ok();
        }
        // Rough ETA from throughput so far. Combos differ a lot in cost by
        // balloon count, so this is only meaningful once a few have landed.
        let elapsed = started.elapsed().as_secs_f64();
        let eta = if n > 0 { elapsed / n as f64 * (total - n) as f64 } else { 0.0 };
        eprintln!(
            "[{n}/{total}] seed={} n={} {} tower={} mesh={}  ({:.0}s elapsed, ~{:.0}m left)",
            combo.seed,
            combo.n,
            ack_name(combo.ack),
            combo.tower_contact,
            combo.mesh_hop,
            elapsed,
            eta / 60.0,
        );
    });

    eprintln!("\ndone in {:.1} min -> {OUT_CSV}", started.elapsed().as_secs_f64() / 60.0);
    eprintln!("coeff={DEFAULT_HORIZON_REFRACTION_COEFF}, zero wind, {rounds} rounds per combo");
}
