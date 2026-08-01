// Do telemetry bundles actually get to the ground, and what happens to the ones
// that don't? The C2 counterpart to beacon_convergence.rs.
//
// Drives a real `World` offline with zero wind. Reports delivery against mesh
// density, then runs the invariant that matters: with every tower removed, every
// bundle in the world must *resolve* — delivered (with its ack independently
// resolving too), dropped, or handed to satellite — with nothing held forever
// and no unbounded growth. C1's protocol bug was exactly this shape (state that
// quietly sustained itself), it was invisible in the UI, and it only surfaced in
// a scenario impossible to stage by clicking. Slice 2 adds acks as a second
// thing that must independently drain: a bundle resolving doesn't mean its ack
// has, so the conservation check below tracks both.
//
//   cargo run --release --bin bundle_delivery [n_balloons] [rounds]

use sim_server::config::*;
use sim_server::sim::{Snapshot, World};
use sim_server::wind_field::WindField;
use std::sync::Arc;

fn advance_round(world: &mut World) -> Snapshot {
    let dt = TICK_DT_SECONDS * TIME_SCALE;
    let mut s = world.tick(dt);
    for _ in 1..COMMS_EVERY_N_TICKS {
        s = world.tick(dt);
    }
    s
}

/// The slot budget, broken down. A bundle only ever gets
/// BUNDLE_MAX_AGE_ROUNDS / BEACON_INTERVAL_ROUNDS wake slots, so where the
/// wasted ones go decides whether it arrives.
fn report_slots(st: &sim_server::bundle::BundleStats) {
    let budget = BUNDLE_MAX_AGE_ROUNDS / BEACON_INTERVAL_ROUNDS;
    let pct = |x: u64| {
        if st.slots_with_bundle == 0 {
            0.0
        } else {
            100.0 * x as f64 / st.slots_with_bundle as f64
        }
    };
    println!(
        "\n  slot budget per bundle: {budget}  |  held-bundle wake slots: {}  |  stall rate {:.1}%",
        st.slots_with_bundle,
        100.0 * st.stall_rate()
    );
    println!(
        "    moved {:>7} ({:>4.1}%)   no belief {:>7} ({:>4.1}%)   stale next hop {:>7} ({:>4.1}%)\
         \n    tower gone {:>4} ({:>4.1}%)   blocked {:>9} ({:>4.1}%)",
        st.slots_used(),
        pct(st.slots_used()),
        st.stall_no_belief,
        pct(st.stall_no_belief),
        st.stall_stale_next_hop,
        pct(st.stall_stale_next_hop),
        st.stall_tower_gone,
        pct(st.stall_tower_gone),
        st.blocked,
        pct(st.blocked),
    );
    // The last-hop ceiling. If demand exceeds this, no amount of queue depth or
    // routing quality can close the gap — the bundles that miss are not being
    // misrouted, there is simply nowhere for them to land in time.
    let demand = if st.rounds_sampled > 0 {
        st.originated as f64 / st.rounds_sampled as f64
    } else {
        0.0
    };
    let served = if st.rounds_sampled > 0 {
        st.delivered as f64 / st.rounds_sampled as f64
    } else {
        0.0
    };
    println!(
        "    tower-adjacent balloons {:.1}  ->  delivery ceiling {:.2}/round  |  \
         originated {:.2}/round  |  delivered {:.2}/round  ({:.0}% of ceiling)",
        st.mean_tower_adjacent(),
        st.delivery_capacity_per_round(),
        demand,
        served,
        if st.delivery_capacity_per_round() > 0.0 {
            100.0 * served / st.delivery_capacity_per_round()
        } else {
            0.0
        },
    );
    println!(
        "    mean believed depth {:.2} hops  |  delivered at {:.2} hops  |  satellite after {:.2} hops",
        sim_server::bundle::hist_mean(&st.belief_hops),
        sim_server::bundle::hist_mean(&st.delivered_hops),
        sim_server::bundle::hist_mean(&st.satellite_hops),
    );
    print!("    belief-depth histogram:");
    for (h, &c) in st.belief_hops.iter().enumerate().take(21) {
        if c > 0 {
            print!(" {h}:{c}");
        }
    }
    println!();
    print!("    delivered-hops histogram:");
    for (h, &c) in st.delivered_hops.iter().enumerate().take(21) {
        if c > 0 {
            print!(" {h}:{c}");
        }
    }
    println!();
    print!("    satellite-hops histogram:");
    for (h, &c) in st.satellite_hops.iter().enumerate().take(21) {
        if c > 0 {
            print!(" {h}:{c}");
        }
    }
    println!();
    // Every delivered bundle's ack independently either completes the reverse
    // path, or doesn't — the two-state model from docs/design/MESH_COMMS_DESIGN.md §4.
    // `still owed` is delivered bundles whose ack hasn't resolved either way
    // yet, same censoring caveat as delivered/originated above.
    let owed = st.delivered.saturating_sub(st.acked + st.ack_lost);
    println!(
        "    acks: delivered {}  ->  acked {} ({:.1}%)  ack_lost {} ({:.1}%)  still owed {}",
        st.delivered,
        st.acked,
        if st.delivered > 0 { 100.0 * st.acked as f64 / st.delivered as f64 } else { 0.0 },
        st.ack_lost,
        if st.delivered > 0 { 100.0 * st.ack_lost as f64 / st.delivered as f64 } else { 0.0 },
        owed,
    );
    println!();
}

fn build(n: u32, coeff: f64) -> World {
    let mut world = World::new(Arc::new(WindField::zero()));
    for &(lon, lat, h) in INITIAL_TOWERS {
        world.add_tower(lon, lat, h);
    }
    world.spawn_balloon_pool(n);
    world.set_visible_count(n);
    world.horizon_refraction_coeff = coeff;
    world
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1200);
    let rounds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);
    // Optional third arg pins phase 1 to a single coefficient, which is what
    // makes sweeping RELAY_QUEUE_CAPACITY cheap enough to actually do.
    let only: Option<f64> = args.get(3).and_then(|s| s.parse().ok());

    // PREFER_NEARER=1 runs the same protocol with hop count, not freshness, as
    // the primary route-selection key. See src/ablation.rs.
    let prefer_nearer = std::env::var("PREFER_NEARER").is_ok_and(|v| v == "1");
    sim_server::ablation::set_prefer_nearer(prefer_nearer);
    println!("route selection: {}", if prefer_nearer { "NEAREST-first (ablation)" } else { "FRESHEST-first (shipped)" });

    println!(
        "n={n}  rounds={rounds}  queue={RELAY_QUEUE_CAPACITY}  \
         originate every {BUNDLE_INTERVAL_ROUNDS}  \
         1 hop = {BEACON_INTERVAL_ROUNDS} rounds ({:.1} real s, {:.0} sim min)\n",
        BEACON_INTERVAL_ROUNDS as f64 * COMMS_EVERY_N_TICKS as f64 * TICK_INTERVAL_MS as f64
            / 1000.0,
        BEACON_INTERVAL_ROUNDS as f64 * COMMS_ROUND_SIM_SECONDS / 60.0,
    );

    // --- Phase 1: delivery vs. mesh density ---------------------------------
    //
    // The prediction from docs/design/MESH_COMMS_DESIGN.md §4: round-trip delivery only
    // works above the percolation threshold (degree ~4.5). Below it, paths are
    // long, beliefs expire mid-flight, and bundles should strand rather than
    // arrive. Delivery ought to collapse sharply, not gracefully.
    println!("--- delivery vs. mesh density ---\n");
    // `deliv/orig` is reported alongside `completion` deliberately. It counts
    // every bundle still legitimately in flight at the cutoff as a failure, so
    // it is censored downward by however much traffic is mid-journey; the gap
    // between the two columns *is* that censoring. `completion` is the honest
    // delivery figure. Both are shown so the bias stays visible.
    println!(
        "{:>6}  {:>7}  {:>10}  {:>10}  {:>9}  {:>9}  {:>9}  {:>10}  {:>10}",
        "coeff",
        "degree",
        "grounded%",
        "delivered",
        "satellite",
        "in-flight",
        "blocked",
        "deliv/orig",
        "completion"
    );
    println!("{}", "-".repeat(96));

    let coeffs: Vec<f64> = only.map_or_else(|| vec![2.5, 3.0, 3.57, 4.12, 5.0], |c| vec![c]);
    for coeff in coeffs {
        let mut world = build(n, coeff);
        let mut last = advance_round(&mut world);
        for _ in 1..rounds {
            last = advance_round(&mut world);
        }
        let st = world.bundle_stats();
        let censored = if st.originated > 0 {
            100.0 * st.delivered as f64 / st.originated as f64
        } else {
            0.0
        };
        println!(
            "{coeff:>6.2}  {:>7.2}  {:>9.1}%  {:>10}  {:>9}  {:>9}  {:>9}  {:>9.1}%  {:>9.1}%",
            last.mean_degree,
            last.grounded_pct,
            st.delivered,
            st.satellite,
            last.bundles_in_flight,
            st.blocked,
            censored,
            100.0 * st.completion_rate(),
        );
        report_slots(&st);
    }

    // --- Phase 2: the resolution invariant ----------------------------------
    //
    // Remove every tower *and* stop origination. Silencing origination is what
    // makes this a drain test: without it balloons keep producing bundles they
    // can never send, in-flight plateaus at the fleet size, and the invariant
    // looks violated when nothing is actually stuck. (The first version of this
    // harness made exactly that mistake.) With no new bundles and no towers,
    // every bundle still in the world must leave circulation.
    println!("\n--- removing every tower: every bundle must resolve ---\n");
    let mut world = build(n, DEFAULT_HORIZON_REFRACTION_COEFF);
    for _ in 0..rounds {
        advance_round(&mut world);
    }
    let before = world.bundle_stats();
    let tower_ids: Vec<u32> = world.towers.iter().map(|t| t.id).collect();
    for id in tower_ids {
        world.remove_tower(id);
    }
    world.halt_origination(); // no new bundles from here on
    println!("at tower removal: {} in flight", {
        let s = advance_round(&mut world);
        s.bundles_in_flight
    });

    let acks_in_flight = |w: &World| -> u64 { w.protocol().acks_in_flight() };

    println!(
        "\n{:>7}  {:>10}  {:>10}  {:>9}  {:>9}",
        "round", "in-flight", "stranded", "resolved", "acks-in-flight"
    );
    // Long enough for the last legitimately-moving bundle to age out, and for
    // its ack (same age budget) to age out too.
    let drain = BUNDLE_MAX_AGE_ROUNDS + BUNDLE_INTERVAL_ROUNDS + 50;
    let mut last_in_flight = u64::MAX;
    let mut last_acks_in_flight = u64::MAX;
    for r in 1..=drain {
        let s = advance_round(&mut world);
        last_in_flight = s.bundles_in_flight;
        last_acks_in_flight = acks_in_flight(&world);
        if r % 50 == 0 || r == drain {
            println!(
                "{r:>7}  {:>10}  {:>10}  {:>9}  {:>9}",
                s.bundles_in_flight,
                s.bundles_stranded,
                world.bundle_stats().resolved(),
                last_acks_in_flight,
            );
        }
    }

    let after = world.bundle_stats();
    println!(
        "\nOriginated {} -> resolved {} (delivered {}, loop {}, ttl {}, satellite {})",
        after.originated,
        after.resolved(),
        after.delivered,
        after.dropped_loop,
        after.dropped_ttl,
        after.satellite,
    );
    println!(
        "Of {} delivered: acked {}, ack_lost {} (owed {})",
        after.delivered,
        after.acked,
        after.ack_lost,
        after.delivered.saturating_sub(after.acked + after.ack_lost),
    );
    println!("Blocked handoffs (retried, not lost): {}", after.blocked);
    println!(
        "Delivered while towers existed: {} (none may be added after removal: {})",
        before.delivered,
        after.delivered - before.delivered,
    );

    // Conservation: nothing may vanish unaccounted. Every bundle ever created is
    // either resolved or still being carried, and every delivered bundle's ack
    // is either resolved (acked or lost) or still in flight.
    let unaccounted = after.originated as i64 - after.resolved() as i64 - last_in_flight as i64;
    let acks_unaccounted =
        after.delivered as i64 - (after.acked + after.ack_lost) as i64 - last_acks_in_flight as i64;
    println!("\nConservation check: originated - resolved - in_flight = {unaccounted} (must be 0)");
    println!("Bundles still in flight with no towers left: {last_in_flight} (must be 0)");
    println!(
        "Ack conservation check: delivered - (acked + ack_lost) - acks_in_flight = {acks_unaccounted} (must be 0)"
    );
    println!("Acks still in flight with no towers left: {last_acks_in_flight} (must be 0)");

    if unaccounted != 0 || last_in_flight != 0 || acks_unaccounted != 0 || last_acks_in_flight != 0
    {
        eprintln!("\nFAILED: bundles or acks are leaking or stuck.");
        std::process::exit(1);
    }
    println!("\nOK: every bundle and every ack resolved, none leaked.");
}
