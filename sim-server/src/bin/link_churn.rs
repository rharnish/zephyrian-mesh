// Does real wind actually churn the topology? Measures link turnover, with a
// zero-wind control.
//
//   cargo run --release --bin link_churn -- [wind] [n] [rounds]
//   cargo run --release --bin link_churn -- 1978-06-09T03:00:00 1200 400
//
// **Why this runs before any protocol experiment using wind.** The intuition
// that "real wind will churn the mesh" does not obviously survive contact with
// how links are defined here. A link exists on *relative* geometry — can these
// two balloons see each other over the horizon — and horizontal wind advects a
// whole region together. A fleet translating at 40 m/s in formation has the
// same topology it started with. The design doc's §1.1 finding (a balloon
// drifts ~0.4% of link range per link round) could survive real weather almost
// unchanged.
//
// What actually breaks links is **shear**: balloons at different altitudes sit
// in different pressure levels and get different velocities, so the fleet
// deforms rather than translating. Whether that is a large effect here depends
// on how far the fleet is spread vertically and on how sheared the particular
// weather is — neither of which is knowable from first principles, and both of
// which this measures.
//
// The number that matters is **half-life**: how many comms rounds until half
// the links present now are gone. dv-dtn's beliefs expire after
// belief_max_age_rounds (60) and a route is used over several rounds, so:
//
//   half-life >> 60 rounds   routes stay valid far longer than they are held;
//                            maintaining one is cheap and replication should
//                            keep losing. Nothing to see.
//   half-life ~ 60 rounds    beliefs and reality decay on the same timescale.
//                            This is the interesting regime.
//   half-life << 60 rounds   routes are stale before they are used, and blind
//                            replication should start winning.

use sim_server::config::{
    COMMS_EVERY_N_TICKS, DEFAULT_HORIZON_REFRACTION_COEFF, INITIAL_TOWERS, TICK_DT_SECONDS,
    TIME_SCALE,
};
use sim_server::sim::World;
use sim_server::wind_cache;
use std::collections::HashSet;
use std::sync::Arc;

struct Churn {
    label: String,
    mean_degree: f64,
    /// Share of links present at one comms round that are gone at the next.
    turnover_per_round: f64,
    /// Rounds until half of a given round's links are gone, from the measured
    /// per-round survival rate. Infinite when nothing ever breaks.
    half_life_rounds: f64,
    /// Mean vertical spread of the fleet — the thing shear acts on. If this is
    /// small, no amount of wind will deform the mesh.
    alt_spread_m: f64,
    /// Spread of wind velocity actually experienced across the fleet. This is
    /// the direct measure of shear: zero means everyone is being pushed the
    /// same way and the topology cannot deform.
    speed_spread_ms: f64,
    mean_speed_ms: f64,
}

fn run(wind_name: &str, n: u32, rounds: u64, seed: u64) -> Result<Churn, String> {
    let field = wind_cache::resolve(wind_name)?;

    // Sample the field over the fleet's altitude band before running, so the
    // shear figure is a property of the weather rather than of where the
    // balloons happened to drift.
    let (mean_speed, speed_spread) = shear_probe(&field);

    let mut world = World::new(Arc::new(field)).with_seed(seed);
    for &(lon, lat, h) in INITIAL_TOWERS {
        world.add_tower(lon, lat, h);
    }
    world.spawn_balloon_pool(n);
    world.set_visible_count(n);

    let dt = TICK_DT_SECONDS * TIME_SCALE;
    let mut prev: Option<HashSet<(u32, u32)>> = None;
    let mut survived = 0u64;
    let mut total = 0u64;
    let mut degree_sum = 0.0;
    let mut degree_n = 0u64;
    let mut alt_spread_sum = 0.0;

    for _ in 0..rounds {
        let mut snapshot = None;
        for _ in 0..COMMS_EVERY_N_TICKS {
            snapshot = Some(world.tick(dt));
        }
        let Some(snap) = snapshot else { continue };

        degree_sum += snap.mean_degree;
        degree_n += 1;

        // Vertical spread of the fleet, as a standard deviation.
        let alts: Vec<f64> = snap.balloons.iter().map(|b| b.alt).collect();
        if alts.len() > 1 {
            let m = alts.iter().sum::<f64>() / alts.len() as f64;
            let var = alts.iter().map(|a| (a - m).powi(2)).sum::<f64>() / (alts.len() - 1) as f64;
            alt_spread_sum += var.sqrt();
        }

        // Balloon-to-balloon links only. Tower links come and go for the same
        // reasons, but the mesh is what a route is built out of.
        let Some(edges) = snap.edges.as_ref() else { continue };
        let now: HashSet<(u32, u32)> = edges
            .iter()
            .filter_map(|e| {
                // pair_key is "b12|t3" / "b12|b40"; keep balloon-balloon only.
                let (a, b) = e.pair_key.split_once('|')?;
                if !(a.starts_with('b') && b.starts_with('b')) {
                    return None;
                }
                let (x, y): (u32, u32) = (a[1..].parse().ok()?, b[1..].parse().ok()?);
                Some(if x < y { (x, y) } else { (y, x) })
            })
            .collect();

        if let Some(before) = prev {
            total += before.len() as u64;
            survived += before.intersection(&now).count() as u64;
        }
        prev = Some(now);
    }

    let survival = if total == 0 { 1.0 } else { survived as f64 / total as f64 };
    let turnover = 1.0 - survival;
    // survival^h = 0.5  ->  h = ln(0.5) / ln(survival)
    let half_life =
        if survival >= 1.0 { f64::INFINITY } else { (0.5f64).ln() / survival.ln() };

    Ok(Churn {
        label: if wind_name.is_empty() || wind_name == "none" {
            "zero wind (control)".into()
        } else {
            wind_name.to_string()
        },
        mean_degree: if degree_n == 0 { 0.0 } else { degree_sum / degree_n as f64 },
        turnover_per_round: turnover,
        half_life_rounds: half_life,
        alt_spread_m: alt_spread_sum / rounds as f64,
        speed_spread_ms: speed_spread,
        mean_speed_ms: mean_speed,
    })
}

/// Wind speed across the altitude band balloons actually occupy, sampled over
/// the domain the towers sit in. Returns (mean speed, sd of speed) — the sd is
/// the shear that matters, since a uniform field moves everything together.
fn shear_probe(field: &sim_server::wind_field::WindField) -> (f64, f64) {
    let mut speeds = Vec::new();
    for &(lon, lat, _) in INITIAL_TOWERS {
        for step in 0..=10 {
            // The band balloons fly in; see config BALLOON_MIN_ALT/MAX_ALT.
            let alt = 15_000.0 + step as f64 * 500.0;
            let (u, v) = field.sample(lon, lat, alt);
            speeds.push((u * u + v * v).sqrt());
        }
    }
    if speeds.is_empty() {
        return (0.0, 0.0);
    }
    let m = speeds.iter().sum::<f64>() / speeds.len() as f64;
    let var = if speeds.len() < 2 {
        0.0
    } else {
        speeds.iter().map(|s| (s - m).powi(2)).sum::<f64>() / (speeds.len() - 1) as f64
    };
    (m, var.sqrt())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let wind = args.first().cloned().unwrap_or_else(|| "none".into());
    let n: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1200);
    let rounds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);

    println!("n={n}  rounds={rounds}  coeff={DEFAULT_HORIZON_REFRACTION_COEFF}");
    println!(
        "one comms round = {:.0} sim-seconds\n",
        TICK_DT_SECONDS * TIME_SCALE * COMMS_EVERY_N_TICKS as f64
    );

    // The control always runs: a churn figure means nothing without the
    // frozen-topology baseline every existing result was measured against.
    let mut rows = Vec::new();
    for name in ["none", wind.as_str()] {
        if rows.len() == 1 && (name == "none" || name.is_empty()) {
            continue; // asked for the control only
        }
        match run(name, n, rounds, 42) {
            Ok(c) => rows.push(c),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }

    println!(
        "{:<24}  {:>7}  {:>9}  {:>11}  {:>10}  {:>12}",
        "wind", "degree", "turnover", "half-life", "alt sd (m)", "shear (m/s)"
    );
    println!("{}", "-".repeat(84));
    for c in &rows {
        let hl = if c.half_life_rounds.is_infinite() {
            "never".to_string()
        } else {
            format!("{:.0} rd", c.half_life_rounds)
        };
        println!(
            "{:<24}  {:>7.2}  {:>8.3}%  {:>11}  {:>10.0}  {:>5.1} ± {:.1}",
            c.label,
            c.mean_degree,
            c.turnover_per_round * 100.0,
            hl,
            c.alt_spread_m,
            c.mean_speed_ms,
            c.speed_spread_ms
        );
    }

    if let (Some(ctrl), Some(w)) = (rows.first(), rows.get(1)) {
        println!();
        let ratio = if ctrl.turnover_per_round <= 0.0 {
            f64::INFINITY
        } else {
            w.turnover_per_round / ctrl.turnover_per_round
        };
        println!(
            "Wind multiplies link turnover by {:.1}x ({:.3}% -> {:.3}% per round).",
            ratio,
            ctrl.turnover_per_round * 100.0,
            w.turnover_per_round * 100.0
        );

        // The decision-relevant quantity is not half-life against the belief
        // lease — a belief expiring is harmless, it just gets replaced. What
        // costs a delivery is a link on the *path in use* breaking while a
        // bundle is still traversing it. A bundle advances one hop per wake
        // slot, so a d-hop path is in transit for d * beacon_interval rounds,
        // and every one of its d links has to survive that long.
        const HOP_ROUNDS: f64 = 5.0; // beacon_interval_rounds
        println!(
            "\nProbability a whole path survives its own transit ({HOP_ROUNDS:.0} rounds/hop):\n"
        );
        println!("{:>6}  {:>9}  {:>14}  {:>14}", "hops", "transit", "zero wind", "under wind");
        println!("{}", "-".repeat(50));
        let mut wind_at_7 = 1.0;
        for d in [3.0, 5.0, 7.0, 10.0] {
            let transit = d * HOP_ROUNDS;
            let p = |turnover: f64| (1.0 - turnover).powf(transit).powf(d);
            let (pc, pw) = (p(ctrl.turnover_per_round), p(w.turnover_per_round));
            if d == 7.0 {
                wind_at_7 = pw;
            }
            println!("{:>6.0}  {:>7.0} rd  {:>13.1}%  {:>13.1}%", d, transit, pc * 100.0, pw * 100.0);
        }

        println!(
            "\nMeasured mean believed depth is ~7 hops, so the 7-hop row is the one to read."
        );
        if wind_at_7 > 0.9 {
            println!(
                "\n=> Paths almost always outlive their own transit. This weather will not\n   \
                 invert the routing-vs-replication ordering; that needs a more sheared field."
            );
        } else if wind_at_7 > 0.5 {
            println!(
                "\n=> A meaningful minority of paths break mid-transit. dv-dtn should lose\n   \
                 real ground here, though probably not enough to fall behind replication."
            );
        } else {
            println!(
                "\n=> Most paths do NOT survive their own transit. This is the regime where\n   \
                 maintaining a route stops paying for itself and replication should close the\n   \
                 gap — the comparison is worth running on this field."
            );
        }
    }
}
