// Do telemetry bundles actually get to the ground, and what happens to the ones
// that don't? The C2 counterpart to beacon_convergence.rs.
//
// Drives a real `World` offline with zero wind. Reports delivery against mesh
// density, then runs the invariant that matters: with every tower removed, every
// bundle in the world must *resolve* — delivered, dropped, or expired — with
// nothing held forever and no unbounded growth. C1's protocol bug was exactly
// this shape (state that quietly sustained itself), it was invisible in the UI,
// and it only surfaced in a scenario impossible to stage by clicking.
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
    // The prediction from MESH_COMMS_DESIGN.md §4: round-trip delivery only
    // works above the percolation threshold (degree ~4.5). Below it, paths are
    // long, beliefs expire mid-flight, and bundles should strand rather than
    // arrive. Delivery ought to collapse sharply, not gracefully.
    println!("--- delivery vs. mesh density ---\n");
    println!(
        "{:>6}  {:>7}  {:>10}  {:>10}  {:>7}  {:>9}  {:>9}  {:>8}",
        "coeff", "degree", "grounded%", "delivered", "expired", "in-flight", "blocked", "deliv/orig"
    );
    println!("{}", "-".repeat(82));

    let coeffs: Vec<f64> =
        only.map_or_else(|| vec![2.5, 3.0, 3.57, 4.12, 5.0], |c| vec![c]);
    for coeff in coeffs {
        let mut world = build(n, coeff);
        let mut last = advance_round(&mut world);
        for _ in 1..rounds {
            last = advance_round(&mut world);
        }
        let st = world.bundle_stats();
        let rate = if st.originated > 0 {
            100.0 * st.delivered as f64 / st.originated as f64
        } else {
            0.0
        };
        println!(
            "{coeff:>6.2}  {:>7.2}  {:>9.1}%  {:>10}  {:>7}  {:>9}  {:>9}  {:>7.1}%",
            last.mean_degree,
            last.grounded_pct,
            st.delivered,
            st.expired,
            last.bundles_in_flight,
            st.blocked,
            rate
        );
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
    for b in world.balloons.iter_mut() {
        b.next_bundle_round = u64::MAX; // no new bundles from here on
    }
    println!("at tower removal: {} in flight", {
        let s = advance_round(&mut world);
        s.bundles_in_flight
    });

    println!("\n{:>7}  {:>10}  {:>10}  {:>9}", "round", "in-flight", "stranded", "resolved");
    // Long enough for the last legitimately-moving bundle to age out.
    let drain = BUNDLE_MAX_AGE_ROUNDS + BUNDLE_INTERVAL_ROUNDS + 50;
    let mut last_in_flight = u64::MAX;
    for r in 1..=drain {
        let s = advance_round(&mut world);
        last_in_flight = s.bundles_in_flight;
        if r % 50 == 0 || r == drain {
            println!(
                "{r:>7}  {:>10}  {:>10}  {:>9}",
                s.bundles_in_flight,
                s.bundles_stranded,
                world.bundle_stats().resolved()
            );
        }
    }

    let after = world.bundle_stats();
    println!(
        "\nOriginated {} -> resolved {} (delivered {}, loop {}, ttl {}, expired {})",
        after.originated,
        after.resolved(),
        after.delivered,
        after.dropped_loop,
        after.dropped_ttl,
        after.expired,
    );
    println!("Blocked handoffs (retried, not lost): {}", after.blocked);
    println!(
        "Delivered while towers existed: {} (none may be added after removal: {})",
        before.delivered,
        after.delivered - before.delivered,
    );

    // Conservation: nothing may vanish unaccounted. Every bundle ever created is
    // either resolved or still being carried.
    let unaccounted =
        after.originated as i64 - after.resolved() as i64 - last_in_flight as i64;
    println!("\nConservation check: originated - resolved - in_flight = {unaccounted} (must be 0)");
    println!("Bundles still in flight with no towers left: {last_in_flight} (must be 0)");

    if unaccounted != 0 || last_in_flight != 0 {
        eprintln!("\nFAILED: bundles are leaking or stuck.");
        std::process::exit(1);
    }
    println!("\nOK: every bundle resolved, none leaked.");
}
