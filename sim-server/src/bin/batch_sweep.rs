// Does batching move delivery, and where does the limit sit afterwards?
//
// §4 of the design doc established that the last hop is the bottleneck above
// percolation: only ~23 of 1200 balloons can hear a tower at any moment, and
// raising `tower_contact` from 1 to 4 bought +16 points before saturating.
// That was one lever on one link. `BatchPolicy` generalizes it to both link
// kinds, so this sweeps the pair and asks the obvious follow-up: once the last
// hop is no longer binding, does letting *mesh* hops carry more than one
// bundle per wake buy anything, or is the mesh limited by opportunity rather
// than by airtime?
//
//   cargo run --release --bin batch_sweep [n_balloons] [rounds]
//
// Zero wind and a fixed seed, so every combo sees the same balloon field and
// the differences are the policy rather than the draw (see World::with_seed).

use sim_server::config::{
    COMMS_EVERY_N_TICKS, DEFAULT_HORIZON_REFRACTION_COEFF, INITIAL_TOWERS, TICK_DT_SECONDS,
    TIME_SCALE,
};
use sim_server::protocol::dv_dtn::params::{AckPolicy, BatchPolicy, DvDtnParams};
use sim_server::protocol::ProtocolSpec;
use sim_server::sim::World;
use sim_server::wind_field::WindField;
use std::sync::Arc;

const SEED: u64 = 42;

fn advance_round(world: &mut World) {
    let dt = TICK_DT_SECONDS * TIME_SCALE;
    for _ in 0..COMMS_EVERY_N_TICKS {
        world.tick(dt);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1200);
    let rounds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);

    println!("n={n}  rounds={rounds}  seed={SEED}  zero wind  coeff={DEFAULT_HORIZON_REFRACTION_COEFF}");
    println!("(mesh_hop=1, tower_contact=4 is what ships)\n");
    println!(
        "{:>5}  {:>7}  {:>10}  {:>10}  {:>11}  {:>9}  {:>9}  {:>9}",
        "mesh", "tower", "delivered", "completion", "deliv/round", "ceiling", "blocked", "ack_lost"
    );
    println!("{}", "-".repeat(85));

    for &(ack_policy, ack_label) in
        &[(AckPolicy::SourceRouted, "source-routed"), (AckPolicy::Digest, "digest")]
    {
        println!("--- acks: {ack_label} ---");
        for &tower_contact in &[1usize, 4] {
            for &mesh_hop in &[1usize, 2, 4, 8] {
                let params = DvDtnParams {
                    batch: BatchPolicy { mesh_hop, tower_contact },
                    ack_policy,
                    ..Default::default()
                };
                let mut world = World::new(Arc::new(WindField::zero()))
                    .with_protocol(ProtocolSpec::DvDtn(params))
                    .with_seed(SEED);
                for &(lon, lat, h) in INITIAL_TOWERS {
                    world.add_tower(lon, lat, h);
                }
                world.spawn_balloon_pool(n);
                world.set_visible_count(n);
                for _ in 0..rounds {
                    advance_round(&mut world);
                }
                let st = world.bundle_stats();
                println!(
                    "{:>5}  {:>7}  {:>10}  {:>9.1}%  {:>11.2}  {:>9.2}  {:>9}  {:>9}",
                    mesh_hop,
                    tower_contact,
                    st.delivered,
                    100.0 * st.completion_rate(),
                    st.delivered as f64 / rounds as f64,
                    st.delivery_capacity_per_round(&params),
                    st.blocked,
                    st.ack_lost,
                );
            }
            println!();
        }
    }

    println!(
        "Read `deliv/round` against `ceiling`: the ceiling is what the last hop alone\n\
         could pass. Delivery well under it means the mesh is the constraint; delivery\n\
         pinned at it means the ground link is."
    );
}
