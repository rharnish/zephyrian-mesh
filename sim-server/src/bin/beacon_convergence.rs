// Does decentralized discovery actually work, and how long does it take?
//
// Drives a real `World` offline with zero wind, so the topology is essentially
// frozen and what we measure is pure beacon propagation rather than propagation
// racing link churn. Reports, per comms round, how many balloons believe they
// have a route versus how many actually do.
//
// Everything here counts in *comms rounds*, not world ticks — discovery steps
// once per round (config::COMMS_EVERY_N_TICKS ticks), so rounds are the unit
// the protocol constants are denominated in and the only unit in which these
// curves are comparable to them.
//
// What to look for:
//   - `believes` should climb from 0 and converge toward `truth` (grounded %).
//   - `unaware` (has a route, hasn't heard) should start at ~truth and decay —
//     that decay curve *is* the discovery wavefront.
//   - `stale` settles around 0.4-1.1% here, not 0. Zero wind freezes latitude
//     and longitude but not altitude, and at 8 ticks per comms round the
//     altitude random walk breaks links slightly faster than beliefs expire.
//     A jump well above that band means something is wrong.
//
//   cargo run --release --bin beacon_convergence [n_balloons] [rounds]

use sim_server::config::{
    BEACON_INTERVAL_ROUNDS, BELIEF_MAX_AGE_ROUNDS, COMMS_EVERY_N_TICKS, COMMS_ROUND_SIM_SECONDS,
    DEFAULT_HORIZON_REFRACTION_COEFF, INITIAL_TOWERS, TICK_INTERVAL_MS,
};
use sim_server::sim::{Snapshot, World};
use sim_server::wind_field::WindField;
use std::sync::Arc;

/// Advance exactly one comms round and return the snapshot at the end of it.
fn advance_round(world: &mut World) -> Snapshot {
    let mut s = world.tick(60.0);
    for _ in 1..COMMS_EVERY_N_TICKS {
        s = world.tick(60.0);
    }
    s
}

/// How long `rounds` comms rounds take on a viewer's wall clock.
fn real_seconds(rounds: u64) -> f64 {
    rounds as f64 * COMMS_EVERY_N_TICKS as f64 * TICK_INTERVAL_MS as f64 / 1000.0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1200);
    let rounds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(80);

    let mut world = World::new(Arc::new(WindField::zero()));
    for &(lon, lat, h) in INITIAL_TOWERS {
        world.add_tower(lon, lat, h);
    }
    world.spawn_balloon_pool(n);
    world.set_visible_count(n);

    println!(
        "n={n}  coeff={DEFAULT_HORIZON_REFRACTION_COEFF}  towers={}  \
         beacon interval={BEACON_INTERVAL_ROUNDS} rounds  belief max age={BELIEF_MAX_AGE_ROUNDS} rounds",
        INITIAL_TOWERS.len()
    );
    println!(
        "(1 round = {COMMS_EVERY_N_TICKS} ticks = {:.0} simulated minutes = {:.1} real seconds; \
         zero wind, so topology is ~frozen)\n",
        COMMS_ROUND_SIM_SECONDS / 60.0,
        real_seconds(1),
    );
    println!(
        "{:>5}  {:>7}  {:>9}  {:>7}  {:>8}   {}",
        "round", "truth%", "believes%", "stale%", "unaware%", "discovery"
    );

    let mut converged_at: Option<u64> = None;
    for round in 1..=rounds {
        let s = advance_round(&mut world);
        // Fraction of the reachable population that has actually been told.
        let discovered = if s.grounded_pct > 0.0 {
            (s.believed_grounded_pct - s.belief_stale_pct) / s.grounded_pct
        } else {
            0.0
        };
        if converged_at.is_none() && discovered >= 0.99 {
            converged_at = Some(round);
        }
        if round <= 20 || round % 5 == 0 {
            let bar = "#".repeat((discovered * 40.0).round().clamp(0.0, 40.0) as usize);
            println!(
                "{:>5}  {:>6.1}%  {:>8.1}%  {:>6.1}%  {:>7.1}%   {}",
                round,
                s.grounded_pct,
                s.believed_grounded_pct,
                s.belief_stale_pct,
                s.belief_unaware_pct,
                bar
            );
        }
    }

    match converged_at {
        Some(t) => println!(
            "\n99% of reachable balloons had learned a route by round {t} \
             (~{:.1} beacon intervals, ~{:.0} simulated minutes, \
             ~{:.1} real seconds on screen).",
            t as f64 / BEACON_INTERVAL_ROUNDS as f64,
            t as f64 * COMMS_ROUND_SIM_SECONDS / 60.0,
            real_seconds(t),
        ),
        None => println!("\nDid not reach 99% discovery within {rounds} rounds."),
    }

    // Phase 2: shatter the mesh instantly by collapsing radio range, and watch
    // belief outlive reality. Truth should crash on the next link recompute
    // while `believes` stays high — those balloons are still confidently
    // routing toward a tower they can no longer reach. The gap is `stale`, and
    // it should drain away over roughly BELIEF_MAX_AGE_ROUNDS as beliefs age out.
    println!("\n--- collapsing horizon coeff to 2.5 (mesh shatters) ---");
    println!(
        "{:>5}  {:>7}  {:>9}  {:>7}  {:>8}   {}",
        "round", "truth%", "believes%", "stale%", "unaware%", "stale"
    );
    world.horizon_refraction_coeff = 2.5;
    let mut peak_stale: f64 = 0.0;
    for round in (rounds + 1)..=(rounds + 40) {
        let s = advance_round(&mut world);
        peak_stale = peak_stale.max(s.belief_stale_pct);
        if round % 2 == 0 {
            let bar = "#".repeat((s.belief_stale_pct * 0.4).round().clamp(0.0, 40.0) as usize);
            println!(
                "{:>5}  {:>6.1}%  {:>8.1}%  {:>6.1}%  {:>7.1}%   {}",
                round,
                s.grounded_pct,
                s.believed_grounded_pct,
                s.belief_stale_pct,
                s.belief_unaware_pct,
                bar
            );
        }
    }
    println!(
        "\nPeak stale belief: {peak_stale:.1}% of balloons were confidently routing \
         toward a tower they could no longer reach."
    );

    // Phase 3: silence every tower. Now no fresh wave exists anywhere in the
    // world, so nothing can legitimately renew a lease and belief must decay
    // to exactly zero. If it plateaus here, beliefs are self-sustaining and
    // the protocol is broken; if it drains, then any plateau in phase 2 is
    // real intermittent contact rather than stale news circulating.
    println!("\n--- removing every tower (no fresh beacons can exist) ---");
    let tower_ids: Vec<u32> = world.towers.iter().map(|t| t.id).collect();
    for id in tower_ids {
        world.remove_tower(id);
    }
    // Run past the max-age horizon — the last beacons emitted just before the
    // towers went away are entitled to live exactly that long.
    let start = rounds + 40;
    for round in (start + 1)..=(start + BELIEF_MAX_AGE_ROUNDS + 20) {
        let s = advance_round(&mut world);
        if round % 10 == 0 {
            println!(
                "{:>5}  {:>6.1}%  {:>8.1}%  {:>6.1}%  {:>7.1}%",
                round,
                s.grounded_pct,
                s.believed_grounded_pct,
                s.belief_stale_pct,
                s.belief_unaware_pct
            );
        }
    }
    let leftover = world.balloons.iter().filter(|b| b.believed_hops.is_some()).count();
    println!(
        "\nBalloons still believing in a route with no towers left in the world: {leftover} \
         (must be 0)."
    );
}
