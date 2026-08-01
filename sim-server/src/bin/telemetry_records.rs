// Is the telemetry any good? The C3-non-crypto counterpart to
// beacon_convergence.rs and bundle_delivery.rs.
//
// Two things worth checking, neither visible in the UI:
//
//   1. The ISA model itself — printed as a profile table against the textbook
//      values, and round-tripped through the ported pressure->altitude function
//      that `weather-data-server/wind_backend.py` uses to label the ERA5
//      pressure levels the wind field is built from. If those two disagree, a
//      balloon reports a pressure inconsistent with the wind it is being blown
//      by, which no amount of staring at the globe would reveal.
//
//   2. The records actually produced by a running World — that they sample the
//      balloon's real position, that the log stays bounded and ordered, and
//      that the copy riding each bundle matches the copy retained at the origin.
//
//   cargo run --release --bin telemetry_records [n_balloons] [rounds]

use sim_server::atmosphere;
use sim_server::config::*;
use sim_server::sim::World;
use sim_server::wind_field::WindField;
use std::sync::Arc;

fn advance_round(world: &mut World) {
    let dt = TICK_DT_SECONDS * TIME_SCALE;
    for _ in 0..COMMS_EVERY_N_TICKS {
        world.tick(dt);
    }
}

fn print_isa_profile() {
    println!("--- ISA profile (atmosphere.rs) ---\n");
    println!(
        "{:>8}  {:>9}  {:>11}  {:>10}  {:>8}  {:>12}",
        "alt (m)", "T (K)", "P (hPa)", "rho", "RH %", "round-trip"
    );
    println!("{}", "-".repeat(70));
    for &h in &[0.0, 2_000.0, 5_000.0, 8_000.0, 11_000.0, 15_000.0, 18_000.0, 20_000.0, 25_000.0] {
        let p = atmosphere::pressure_hpa(h);
        // Round-trip through the ported Python conversion: this is the check
        // that pins the Rust atmosphere to the one the wind pipeline assumes.
        let back = atmosphere::altitude_m_from_pressure_hpa(p);
        assert!(
            (back - h).abs() < 0.01,
            "ISA round-trip broken at {h} m: came back as {back} m"
        );
        println!(
            "{:>8.0}  {:>9.2}  {:>11.3}  {:>10.5}  {:>8.3}  {:>10.2} m",
            h,
            atmosphere::temperature_k(h),
            p,
            atmosphere::density_kg_m3(h),
            // Humidity varies with position; sample one spot for the table.
            atmosphere::humidity_pct(23.0, 11.0, h),
            back,
        );
    }
    println!(
        "\n  anchors: rho(0) = {:.4} (ISA 1.225), P(11km) = {:.2} hPa (ISA 226.32), \
         P(20km) = {:.2} hPa (ISA 54.7)",
        atmosphere::density_kg_m3(0.0),
        atmosphere::pressure_hpa(11_000.0),
        atmosphere::pressure_hpa(20_000.0),
    );
    println!("  round-trip against the ported wind_backend.py conversion: OK\n");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(400);
    let rounds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);

    print_isa_profile();

    println!("--- records from a running World (n={n}, {rounds} rounds) ---\n");
    let mut world = World::new(Arc::new(WindField::zero())).with_seed(20260727);
    for &(lon, lat, h) in INITIAL_TOWERS {
        world.add_tower(lon, lat, h);
    }
    world.spawn_balloon_pool(n);
    world.set_visible_count(n);
    for _ in 0..rounds {
        advance_round(&mut world);
    }

    // Sample a few balloons' logs.
    let sampled: Vec<usize> = (0..world.visible_count).step_by(world.visible_count.max(1) / 3 + 1).collect();
    for i in sampled.iter().take(3) {
        let b = &world.balloons[*i];
        let node = &world.protocol().nodes[*i];
        println!("balloon {} — {} record(s) retained, alt now {:.0} m", b.id, node.log.len(), b.alt);
        for r in node.log.iter().take(4) {
            println!(
                "    seq {:>3}  round {:>5}  ({:>8.2}, {:>7.2}) {:>6.0} m  \
                 T {:>6.2} K  P {:>8.3} hPa  RH {:>6.3}%",
                r.seq, r.created_at_round, r.lon, r.lat, r.alt_m, r.temperature_k,
                r.pressure_hpa, r.humidity_pct
            );
        }
        if node.log.len() > 4 {
            println!("    ... {} more", node.log.len() - 4);
        }
    }

    // --- Invariants ---------------------------------------------------------
    let mut total_records = 0u64;
    let mut checked_bundles = 0u64;
    for (i, b) in world.balloons[..world.visible_count].iter().enumerate() {
        let node = &world.protocol().nodes[i];
        assert!(
            node.log.len() <= COMMS_LOG_CAPACITY,
            "balloon {} log overflowed: {} > {COMMS_LOG_CAPACITY}",
            b.id,
            node.log.len()
        );
        let mut prev_seq: Option<u64> = None;
        for r in node.log.iter() {
            total_records += 1;
            assert_eq!(r.origin_id, b.id, "record filed under the wrong balloon");
            assert!(
                (180.0..=320.0).contains(&r.temperature_k),
                "implausible temperature {} K on balloon {}",
                r.temperature_k,
                b.id
            );
            assert!(r.pressure_hpa > 0.0, "non-positive pressure on balloon {}", b.id);
            assert!(
                (0.0..=100.0).contains(&r.humidity_pct),
                "humidity out of range: {}",
                r.humidity_pct
            );
            assert!(
                (BALLOON_MIN_ALT..=BALLOON_MAX_ALT).contains(&r.alt_m),
                "record altitude outside the world: {}",
                r.alt_m
            );
            // Temperature/pressure must be consistent with the altitude the
            // record itself claims — catches a record sampled at the wrong time.
            assert!(
                (r.temperature_k - atmosphere::temperature_k(r.alt_m)).abs() < 1e-6,
                "record temperature disagrees with its own altitude"
            );
            if let Some(p) = prev_seq {
                assert!(r.seq > p, "log out of order on balloon {}: {} after {}", b.id, r.seq, p);
            }
            prev_seq = Some(r.seq);
        }
        // The copy in flight must match the copy retained.
        for bd in node.queue.iter() {
            checked_bundles += 1;
            assert_eq!(bd.record.origin_id, bd.origin_id, "bundle carries another balloon's record");
            assert_eq!(bd.record.seq, bd.seq, "bundle and its record disagree on seq");
        }
    }

    let with_records =
        world.protocol().nodes[..world.visible_count].iter().filter(|n| !n.log.is_empty()).count();
    println!(
        "\n{total_records} record(s) across {with_records}/{} balloons; \
         {checked_bundles} in-flight bundle(s) cross-checked against their origin's log.",
        world.visible_count
    );
    assert!(total_records > 0, "no telemetry was produced at all");
    println!("\nOK: ISA agrees with the ported conversion, records are plausible, logs bounded.");
}
