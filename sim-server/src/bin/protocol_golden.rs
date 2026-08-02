// Byte-for-byte behavioral fingerprint of the comms protocol, for refactors.
//
// This exists to answer exactly one question: did a change to how the protocol
// is *structured* alter what it *does*? Capture its output before the refactor,
// diff it after, and any difference at all is a behavior change:
//
//   cargo run --release --bin protocol_golden > /tmp/golden-before.txt
//   ...refactor...
//   cargo run --release --bin protocol_golden | diff /tmp/golden-before.txt -
//
// Everything here is pinned so the only thing that can move the output is the
// protocol itself: fixed seed, zero wind (no network fetch, frozen topology),
// fixed balloon count and round count. Floats print via `{:?}`, which is the
// shortest round-trippable form — a *bit* of drift shows up rather than being
// rounded away, which is the point. Nothing wall-clock-derived is printed
// (notably not Snapshot::server_time_ms, which changes every run by design).
//
// This is deliberately not a #[test]: it prints a fingerprint for a human to
// diff across a working-tree change, rather than asserting against a committed
// expectation that would need regenerating every time the protocol is tuned.

use sim_server::config::{
    COMMS_EVERY_N_TICKS, DEFAULT_HORIZON_REFRACTION_COEFF, INITIAL_TOWERS, TICK_DT_SECONDS,
    TIME_SCALE,
};
use sim_server::sim::{Snapshot, World};
use sim_server::wind_field::WindField;
use std::sync::Arc;

const SEED: u64 = 42;
const ROUNDS: u64 = 300;

/// Two densities, because they exercise disjoint halves of the protocol. At 400
/// the mesh is below percolation (degree ~2.1): almost everything stalls on
/// `stall_no_belief` and leaves via satellite. At 1200 it is above (degree ~6):
/// bundles actually route multiple hops, queues fill, and the loop/TTL/blocked
/// paths finally execute. A fingerprint from either one alone would leave half
/// the code uncovered.
const SCENARIOS: &[(&str, u32)] = &[("sparse", 400), ("dense", 1200)];

/// Advance exactly one comms round — same shape as the other harnesses, so
/// this samples the protocol on its own clock rather than the tick clock.
fn advance_round(world: &mut World) -> Snapshot {
    let dt = TICK_DT_SECONDS * TIME_SCALE;
    let mut s = world.tick(dt);
    for _ in 1..COMMS_EVERY_N_TICKS {
        s = world.tick(dt);
    }
    s
}

fn main() {
    println!("# protocol golden fingerprint");
    println!("# seed={SEED} rounds={ROUNDS} towers={}", INITIAL_TOWERS.len());
    println!("# coeff={DEFAULT_HORIZON_REFRACTION_COEFF:?} zero-wind");
    for &(label, n) in SCENARIOS {
        run_scenario(label, n);
    }
}

fn run_scenario(label: &str, n_balloons: u32) {
    let mut world = World::new(Arc::new(WindField::zero())).with_seed(SEED);
    for &(lon, lat, h) in INITIAL_TOWERS {
        world.add_tower(lon, lat, h);
    }
    world.spawn_balloon_pool(n_balloons);
    world.set_visible_count(n_balloons);

    println!();
    println!("## scenario={label} n={n_balloons}");

    for round in 0..ROUNDS {
        let s = advance_round(&mut world);
        println!(
            "r{:04} tick={} deg={:?} grounded={:?} believes={:?} stale={:?} unaware={:?} \
             deliv={} lost={} inflight={} stranded={}",
            round,
            s.tick,
            s.mean_degree,
            s.grounded_pct,
            s.believed_grounded_pct,
            s.belief_stale_pct,
            s.belief_unaware_pct,
            s.bundles_delivered,
            s.bundles_lost,
            s.bundles_in_flight,
            s.bundles_stranded,
        );
    }

    let st = world.bundle_stats();
    println!("# final bundle stats [{label}]");
    println!("originated={}", st.originated);
    println!("delivered={}", st.delivered);
    println!("satellite={}", st.satellite);
    println!("dropped_loop={}", st.dropped_loop);
    println!("dropped_ttl={}", st.dropped_ttl);
    println!("blocked={}", st.blocked);
    println!("acked={}", st.acked);
    println!("ack_lost={}", st.ack_lost);
    println!("slots_with_bundle={}", st.slots_with_bundle);
    println!("stall_no_belief={}", st.stall_no_belief);
    println!("stall_stale_next_hop={}", st.stall_stale_next_hop);
    println!("stall_tower_gone={}", st.stall_tower_gone);
    println!("delivered_hops={:?}", st.delivered_hops);
    println!("satellite_hops={:?}", st.satellite_hops);
    println!("belief_hops={:?}", st.belief_hops);
    // Timing, pinned as exact sums and counts rather than as means, so a
    // regression shows up as a changed integer instead of hiding inside a
    // rounded float. Until these lines existed the fingerprint could not tell
    // a refactor that delivered the same bundles *later* from one that changed
    // nothing — which, for a delay-tolerant protocol, is the regression most
    // worth catching.
    println!(
        "delivery_latency=sum:{} n:{}",
        st.delivery_latency.sum, st.delivery_latency.count
    );
    println!("ack_latency=sum:{} n:{}", st.ack_latency.sum, st.ack_latency.count);
    println!(
        "first_hop_latency=sum:{} n:{}",
        st.first_hop_latency.sum, st.first_hop_latency.count
    );
}
