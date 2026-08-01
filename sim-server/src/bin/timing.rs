// Print the simulation's timing model in seconds, derived from config.rs.
//
// Everything is scheduled in ticks, which suits the code and defeats reasoning.
// This binary is the arithmetic, computed from the live constants so it cannot
// go stale the way a hand-written table does. `docs/design/TIMING_MODEL.md` is the prose
// version — if the two disagree, this one is right and the doc needs updating.
//
//   cargo run --release --bin timing

use sim_server::config::*;
use sim_server::protocol::dv_dtn::params::DvDtnParams;

/// The one number here that is measured rather than derived: rounds for a
/// beacon wave to reach 99% of a 1200-balloon field, from
/// `cargo run --release --bin beacon_convergence`. Re-measure if the protocol
/// or the default topology changes.
const MEASURED_CONVERGENCE_ROUNDS: u64 = 38;

fn real_secs_per_tick() -> f64 {
    TICK_INTERVAL_MS as f64 / 1000.0
}

fn sim_secs_per_tick() -> f64 {
    TICK_DT_SECONDS * TIME_SCALE
}

/// Render a simulated duration in whatever unit reads most naturally.
fn fmt_sim(secs: f64) -> String {
    let mins = secs / 60.0;
    if mins < 1.0 {
        format!("{secs:.0} s")
    } else if mins < 120.0 {
        format!("{mins:.0} min")
    } else {
        format!("{:.1} h", mins / 60.0)
    }
}

fn fmt_real(secs: f64) -> String {
    if secs < 1.0 {
        format!("{secs:.2} s")
    } else {
        format!("{secs:.1} s")
    }
}

/// One row: a duration expressed in ticks, shown in both clocks.
fn row(label: &str, constant: &str, ticks: f64) {
    println!(
        "{label:<28}  {constant:<30}  {:>10}  {:>10}",
        fmt_real(ticks * real_secs_per_tick()),
        fmt_sim(ticks * sim_secs_per_tick()),
    );
}

fn main() {
    let rt = real_secs_per_tick();
    let st = sim_secs_per_tick();
    let comms = COMMS_EVERY_N_TICKS as f64;
    // The protocol's own pacing constants live on its params now, not in
    // config.rs. Read them so this report can't drift from what actually runs.
    let params = DvDtnParams::default();
    let beacon_interval = params.beacon_interval_rounds;
    let beacon_jitter = params.beacon_jitter_rounds;
    let belief_max_age = params.belief_max_age_rounds;

    println!("\n=== Three clocks ===\n");
    println!(
        "1 tick        = {} ms real   = {} s simulated",
        TICK_INTERVAL_MS,
        st as u64
    );
    println!(
        "1 link round  = {} ticks  = {} real = {}",
        LINK_UPDATE_EVERY_N_TICKS,
        fmt_real(LINK_UPDATE_EVERY_N_TICKS as f64 * rt),
        fmt_sim(LINK_UPDATE_EVERY_N_TICKS as f64 * st)
    );
    println!(
        "1 comms round = {} ticks  = {} real = {}",
        COMMS_EVERY_N_TICKS,
        fmt_real(comms * rt),
        fmt_sim(comms * st)
    );
    println!("\nThe simulation runs at {:.0}x real time.", st / rt);
    println!(
        "1 real second = {:.0} ticks = {}.",
        1.0 / rt,
        fmt_sim(st / rt)
    );

    println!("\n=== Everything in seconds ===\n");
    println!(
        "{:<28}  {:<30}  {:>10}  {:>10}",
        "Parameter", "Config constant", "Real", "Simulated"
    );
    println!("{}", "-".repeat(84));
    row("Snapshot / frame", &format!("TICK_INTERVAL_MS = {TICK_INTERVAL_MS}"), 1.0);
    row(
        "Link recompute",
        &format!("LINK_UPDATE_EVERY_N_TICKS = {LINK_UPDATE_EVERY_N_TICKS}"),
        LINK_UPDATE_EVERY_N_TICKS as f64,
    );
    row(
        "Comms round",
        &format!("COMMS_EVERY_N_TICKS = {COMMS_EVERY_N_TICKS}"),
        comms,
    );
    row(
        "Beacon transmission",
        &format!("beacon_interval_rounds = {beacon_interval}"),
        beacon_interval as f64 * comms,
    );
    row(
        "Beacon jitter",
        &format!("beacon_jitter_rounds = +/-{beacon_jitter}"),
        beacon_jitter as f64 * comms,
    );
    row(
        "Belief expiry",
        &format!("belief_max_age_rounds = {belief_max_age}"),
        belief_max_age as f64 * comms,
    );
    row(
        "Full discovery (1200)",
        &format!("~{MEASURED_CONVERGENCE_ROUNDS} rounds, measured"),
        MEASURED_CONVERGENCE_ROUNDS as f64 * comms,
    );
    println!(
        "\nbeacon_max_hops = {} is omitted: a hop budget, not a duration.",
        params.beacon_max_hops
    );

    println!("\nIn plain terms: a balloon speaks every {}; a belief it cannot", fmt_real(beacon_interval as f64 * comms * rt));
    println!(
        "refresh dies after {}; a beacon crosses the planet in about {}.",
        fmt_real(belief_max_age as f64 * comms * rt),
        fmt_real(MEASURED_CONVERGENCE_ROUNDS as f64 * comms * rt)
    );

    println!("\n=== Conversion rules ===\n");
    println!("real seconds    = rounds x {:.2}", comms * rt);
    println!("simulated mins  = rounds x {:.0}", comms * st / 60.0);

    println!("\n=== Tuning COMMS_EVERY_N_TICKS ===\n");
    println!(
        "{:>8}  {:>14}  {:>18}  {:>15}  {:>14}",
        "value", "beacon every", "belief dies after", "full discovery", "belief age (sim)"
    );
    println!("{}", "-".repeat(78));
    for candidate in [2u64, 4, 8, 12, 16, 24] {
        let c = candidate as f64;
        let marker = if candidate == COMMS_EVERY_N_TICKS { " <- current" } else { "" };
        println!(
            "{:>8}  {:>14}  {:>18}  {:>15}  {:>14}{}",
            candidate,
            fmt_real(beacon_interval as f64 * c * rt),
            fmt_real(belief_max_age as f64 * c * rt),
            fmt_real(MEASURED_CONVERGENCE_ROUNDS as f64 * c * rt),
            fmt_sim(belief_max_age as f64 * c * st),
            marker
        );
    }
    println!(
        "\nThe last column is the plausibility check: a duty-cycled HAB radio holding a\n\
         route belief for a day is a stretch. If real-time pacing and simulated\n\
         plausibility pull apart, lower TIME_SCALE (currently {TIME_SCALE:.0}) to compensate --\n\
         at the cost of balloons drifting proportionally slower on screen.\n"
    );
}
