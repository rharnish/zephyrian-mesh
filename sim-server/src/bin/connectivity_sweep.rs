// Rust port of experiments/connectivity-sweep.mjs — same model, same sweep
// grid, same output format (JSON per combo + combined CSV), so results are
// directly comparable to the JS run already checked into
// experiments/results/ and experiments/connectivity-sweep-results.csv.
// Reuses sim-server's already-ported physics/link-detection modules as a
// library (see ../lib.rs) rather than reimplementing them.
//
// Writes to SEPARATE output paths (experiments/results-rust/,
// experiments/connectivity-sweep-results-rust.csv) so the existing JS
// results aren't touched — this run exists to compare wall-clock time and
// (ideally) confirm matching output, not to replace the JS data.
//
// Usage (run from the sim-server/ directory, or repo root — paths below
// are relative to cwd, matching connectivity-sweep.mjs's convention of
// being run from the repo root):
//   cargo run --release --bin connectivity_sweep -- bench
//   cargo run --release --bin connectivity_sweep -- quick
//   cargo run --release --bin connectivity_sweep -- full [shardIndex] [shardCount]
//   cargo run --release --bin connectivity_sweep -- combine [outPath]
//
// Uses real wind data fetched once at startup from wind_backend.py (same
// endpoint sim-server's main binary uses), falling back to zero wind if
// that fetch fails — matching main.rs's fetch_wind_field behavior. Balloons
// still hold their spawn (lon, lat) *only* under the zero-wind fallback;
// with real wind loaded, they drift laterally like they do in the live app.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use sim_server::balloon::Balloon;
use sim_server::config::{BALLOON_MAX_ALT, BALLOON_MIN_ALT, GRID_CELL_SIZE_DEG, INITIAL_TOWERS, WIND_API_URL};
use sim_server::geo::{precompute, random_global_position, Precomputed};
use sim_server::link_detection::ConnectivityScratch;
use sim_server::spatial_grid::SpatialGrid;
use sim_server::tower::Tower;
use sim_server::wind_field::WindField;
use std::time::Instant;

const PAYLOAD_INTERVAL_SEC: i64 = 5 * 60; // fixed, not swept
const FIXED_ACK_DURATION_SEC: i64 = 30; // taken out of the sweep, same as JS

const HORIZON_COEFFS: [f64; 4] = [3.4, 3.6, 3.8, 4.0];
const N_BALLOONS: [u32; 6] = [50, 100, 200, 400, 800, 1600];
const FALLBACK_TIMEOUT_MIN: [i64; 4] = [10, 20, 30, 60];

const RESULTS_DIR: &str = "experiments/results-rust";

struct Combo {
    horizon_coeff: f64,
    n_balloons: u32,
    fallback_timeout_min: i64,
    ack_duration_sec: i64,
}

// Deterministic per-combo seed (FNV-1a over the combo's parameters), so each
// combo gets its own balloon layout/altitude-drift draw — closer in spirit
// to the JS sweep's unseeded Math.random() (a fresh draw per run) — while
// staying reproducible across repeated runs of this binary, unlike JS's
// version which can't be replayed exactly. A single shared fixed seed
// across all combos (the previous approach) would have made every combo's
// balloon layout identical, silently correlating results across the sweep.
fn seed_for_combo(combo: &Combo) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mix = |h: u64, x: u64| (h ^ x).wrapping_mul(FNV_PRIME);
    let mut h = FNV_OFFSET;
    h = mix(h, combo.horizon_coeff.to_bits());
    h = mix(h, combo.n_balloons as u64);
    h = mix(h, combo.fallback_timeout_min as u64);
    h = mix(h, combo.ack_duration_sec as u64);
    h
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResultRow {
    horizon_coeff: f64,
    n_balloons: u32,
    fallback_timeout_min: i64,
    ack_duration_sec: i64,
    total_payloads: u64,
    radio_delivered: u64,
    satellite_delivered: u64,
    pct_radio: f64,
    pct_satellite: f64,
}

fn make_balloons(n: u32, rng: &mut impl Rng) -> Vec<Balloon> {
    (0..n)
        .map(|i| {
            let (lon, lat) = random_global_position(rng);
            let alt = BALLOON_MIN_ALT + rng.gen_range(0.0..(BALLOON_MAX_ALT - BALLOON_MIN_ALT));
            Balloon::new(i, lon, lat, alt)
        })
        .collect()
}

fn make_towers() -> Vec<Tower> {
    INITIAL_TOWERS
        .iter()
        .enumerate()
        .map(|(i, &(lon, lat, height_m))| Tower::new(i as u32, lon, lat, height_m))
        .collect()
}

struct BalloonState {
    pending: Vec<i64>,      // createdAt (sim seconds) of not-yet-delivered payloads
    connected_since: Option<i64>, // sim time the current continuous grounded streak began
}

// One simulation run for one parameter combination. Mirrors runOne() in
// connectivity-sweep.mjs line for line — see that file's header comment for
// the full model description (payload generation, radio ack streak,
// satellite fallback timeout).
fn run_one(
    seed: u64,
    wind: &WindField,
    horizon_coeff: f64,
    n_balloons: u32,
    fallback_timeout_sec: i64,
    ack_duration_sec: i64,
    duration_sec: i64,
) -> (u64, u64, u64) {
    let mut rng = StdRng::seed_from_u64(seed);
    let balloons_vec = make_balloons(n_balloons, &mut rng);
    let mut balloons = balloons_vec;
    let towers = make_towers();
    let mut grid = SpatialGrid::new(GRID_CELL_SIZE_DEG);

    // Towers are fixed for the whole run — precompute their trig once
    // instead of every simulated second inside the connectivity check.
    let tower_pre: Vec<Precomputed> =
        towers.iter().map(|t| precompute(t.lon, t.lat, t.height_m, horizon_coeff)).collect();
    let mut scratch = ConnectivityScratch::new(balloons.len(), towers.len());

    let mut state: Vec<BalloonState> = balloons
        .iter()
        .map(|_| BalloonState { pending: Vec::new(), connected_since: None })
        .collect();

    let mut total_payloads: u64 = 0;
    let mut radio_delivered: u64 = 0;
    let mut satellite_delivered: u64 = 0;

    let mut next_payload_gen: i64 = 0;

    for t in 0..duration_sec {
        for b in &mut balloons {
            b.step(1.0, wind, &mut rng);
        }

        // Recomputed every simulated second, unconditionally — same
        // reasoning as the JS version: throttling this by ack duration
        // biases longer-ack combos toward missing brief disconnects.
        let grounded_ids =
            scratch.grounded_balloon_ids(&balloons, &towers, &tower_pre, &mut grid, horizon_coeff);

        if t >= next_payload_gen {
            for s in &mut state {
                s.pending.push(t);
                total_payloads += 1;
            }
            next_payload_gen = t + PAYLOAD_INTERVAL_SEC;
        }

        for (i, b) in balloons.iter().enumerate() {
            let s = &mut state[i];
            let grounded = grounded_ids.contains(&b.id);

            if grounded {
                if s.connected_since.is_none() {
                    s.connected_since = Some(t);
                }
                let streak = t - s.connected_since.unwrap();
                if streak >= ack_duration_sec && !s.pending.is_empty() {
                    radio_delivered += s.pending.len() as u64;
                    s.pending.clear();
                }
            } else {
                s.connected_since = None;
            }

            if !s.pending.is_empty() {
                let cutoff = t - fallback_timeout_sec;
                let mut kept = Vec::with_capacity(s.pending.len());
                for &p in &s.pending {
                    if p > cutoff {
                        kept.push(p);
                    } else {
                        satellite_delivered += 1;
                    }
                }
                s.pending = kept;
            }
        }
    }

    (total_payloads, radio_delivered, satellite_delivered)
}

fn run_combo(combo: &Combo, wind: &WindField, duration_sec: i64) -> ResultRow {
    let fallback_timeout_sec = combo.fallback_timeout_min * 60;
    let (total_payloads, radio_delivered, satellite_delivered) = run_one(
        seed_for_combo(combo),
        wind,
        combo.horizon_coeff,
        combo.n_balloons,
        fallback_timeout_sec,
        combo.ack_duration_sec,
        duration_sec,
    );
    let pct_radio = if total_payloads > 0 { 100.0 * radio_delivered as f64 / total_payloads as f64 } else { 0.0 };
    let pct_satellite =
        if total_payloads > 0 { 100.0 * satellite_delivered as f64 / total_payloads as f64 } else { 0.0 };
    ResultRow {
        horizon_coeff: combo.horizon_coeff,
        n_balloons: combo.n_balloons,
        fallback_timeout_min: combo.fallback_timeout_min,
        ack_duration_sec: combo.ack_duration_sec,
        total_payloads,
        radio_delivered,
        satellite_delivered,
        pct_radio,
        pct_satellite,
    }
}

fn combos() -> Vec<Combo> {
    let mut out = Vec::new();
    for &horizon_coeff in &HORIZON_COEFFS {
        for &n_balloons in &N_BALLOONS {
            for &fallback_timeout_min in &FALLBACK_TIMEOUT_MIN {
                out.push(Combo { horizon_coeff, n_balloons, fallback_timeout_min, ack_duration_sec: FIXED_ACK_DURATION_SEC });
            }
        }
    }
    out
}

fn combo_file_name(combo: &Combo) -> String {
    let h = format!("{:.2}", combo.horizon_coeff).replace('.', "p");
    format!("h{h}_n{}_t{}_a{}.json", combo.n_balloons, combo.fallback_timeout_min, combo.ack_duration_sec)
}

fn write_combo_json(combo: &Combo, row: &ResultRow) -> std::io::Result<String> {
    std::fs::create_dir_all(RESULTS_DIR)?;
    let path = format!("{RESULTS_DIR}/{}", combo_file_name(combo));
    std::fs::write(&path, serde_json::to_string_pretty(row).unwrap())?;
    Ok(path)
}

fn combine_json_to_csv(out_path: &str) -> std::io::Result<usize> {
    let mut rows: Vec<ResultRow> = Vec::new();
    for entry in std::fs::read_dir(RESULTS_DIR)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path)?;
            rows.push(serde_json::from_str(&content).unwrap());
        }
    }
    rows.sort_by(|a, b| {
        a.horizon_coeff
            .partial_cmp(&b.horizon_coeff)
            .unwrap()
            .then(a.n_balloons.cmp(&b.n_balloons))
            .then(a.fallback_timeout_min.cmp(&b.fallback_timeout_min))
    });
    let mut out = String::from(
        "horizonCoeff,nBalloons,fallbackTimeoutMin,ackDurationSec,totalPayloads,radioDelivered,satelliteDelivered,pctRadio,pctSatellite\n",
    );
    for r in &rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{:.3},{:.3}\n",
            r.horizon_coeff,
            r.n_balloons,
            r.fallback_timeout_min,
            r.ack_duration_sec,
            r.total_payloads,
            r.radio_delivered,
            r.satellite_delivered,
            r.pct_radio,
            r.pct_satellite
        ));
    }
    std::fs::write(out_path, &out)?;
    Ok(rows.len())
}

// Fetches real wind data from wind_backend.py, same endpoint and same
// fallback-to-zero-wind-on-failure behavior as main.rs's fetch_wind_field
// — but blocking, since this binary's main() isn't async/tokio like the
// server's is. Uses an explicit generous timeout: the plain
// reqwest::blocking::get() shorthand times out around 30s by default,
// well short of this endpoint's measured ~55s response time (see the
// wind_backend_perf memory) — without this, every run would silently and
// confusingly fall back to zero wind.
fn fetch_wind_field_blocking() -> WindField {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("failed to build HTTP client");
    match client.get(WIND_API_URL).send() {
        Ok(resp) => match resp.json::<WindField>() {
            Ok(wind) => {
                println!("Loaded real wind field from {WIND_API_URL}");
                wind
            }
            Err(e) => {
                eprintln!("Failed to parse wind field response, using zero wind: {e}");
                WindField::zero()
            }
        },
        Err(e) => {
            eprintln!(
                "Failed to fetch wind field from {WIND_API_URL} (is wind_backend.py running?), using zero wind: {e:?}"
            );
            WindField::zero()
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("bench");

    match mode {
        "bench" => {
            let wind = fetch_wind_field_blocking();
            // Worst-case single combo (max balloons) over a short duration,
            // to extrapolate full-sweep cost before committing to it.
            let duration_sec = 60 * 60; // 1 sim hour
            let combo = Combo {
                horizon_coeff: 4.0,
                n_balloons: *N_BALLOONS.iter().max().unwrap(),
                fallback_timeout_min: 30,
                ack_duration_sec: FIXED_ACK_DURATION_SEC,
            };
            println!(
                "Benchmarking worst case (n={}, ackDurationSec={}) over {} sim hour(s)...",
                combo.n_balloons,
                combo.ack_duration_sec,
                duration_sec / 3600
            );
            let t0 = Instant::now();
            let row = run_combo(&combo, &wind, duration_sec);
            let wall_sec = t0.elapsed().as_secs_f64();
            println!(
                "Result: totalPayloads={} radioDelivered={} satelliteDelivered={} pctRadio={:.1} pctSatellite={:.1}",
                row.total_payloads, row.radio_delivered, row.satellite_delivered, row.pct_radio, row.pct_satellite
            );
            println!(
                "Wall time: {:.1}s for 1 sim hour => {:.0}s ({:.1} min) per 24-sim-hour combo at worst case.",
                wall_sec,
                wall_sec * 24.0,
                wall_sec * 24.0 / 60.0
            );
            println!("Total combos in full sweep: {}", HORIZON_COEFFS.len() * N_BALLOONS.len() * FALLBACK_TIMEOUT_MIN.len());
        }
        "quick" => {
            let wind = fetch_wind_field_blocking();
            let duration_sec = 2 * 60 * 60; // 2 sim hours
            println!("{:<10} {:<12} {:<14} {:<10} {:<12}", "nBalloons", "horizonCoeff", "totalPayloads", "pctRadio", "pctSatellite");
            for &n_balloons in &[50, 200] {
                for &horizon_coeff in &[3.4, 4.0] {
                    let combo = Combo { horizon_coeff, n_balloons, fallback_timeout_min: 20, ack_duration_sec: FIXED_ACK_DURATION_SEC };
                    let row = run_combo(&combo, &wind, duration_sec);
                    println!(
                        "{:<10} {:<12} {:<14} {:<10.1} {:<12.1}",
                        row.n_balloons, row.horizon_coeff, row.total_payloads, row.pct_radio, row.pct_satellite
                    );
                }
            }
        }
        "full" => {
            let wind = fetch_wind_field_blocking();
            let shard_index: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(0);
            let shard_count: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(1);
            let duration_sec = 24 * 60 * 60; // 24 sim hours, matching the JS sweep
            let all: Vec<Combo> = combos().into_iter().enumerate().filter(|(i, _)| i % shard_count == shard_index).map(|(_, c)| c).collect();
            println!(
                "Running full sweep shard {shard_index}/{shard_count}: {} combinations x {} sim hours each.",
                all.len(),
                duration_sec / 3600
            );
            std::fs::create_dir_all(RESULTS_DIR).unwrap();
            let t0 = Instant::now();
            let mut done = 0usize;
            for (i, combo) in all.iter().enumerate() {
                let path = format!("{RESULTS_DIR}/{}", combo_file_name(combo));
                if std::path::Path::new(&path).exists() {
                    println!("[shard {shard_index}] [{}/{}] already done, skipping: {path}", i + 1, all.len());
                    continue;
                }
                let row = run_combo(combo, &wind, duration_sec);
                write_combo_json(combo, &row).unwrap();
                done += 1;
                let elapsed = t0.elapsed().as_secs_f64();
                let rate = done as f64 / elapsed;
                let eta_sec = if rate > 0.0 { (all.len() - (i + 1)) as f64 / rate } else { 0.0 };
                println!(
                    "[shard {shard_index}] [{}/{}] wrote {path} — elapsed {:.0}s, ETA {:.1} min",
                    i + 1,
                    all.len(),
                    elapsed,
                    eta_sec / 60.0
                );
            }
            println!("Shard {shard_index} done.");
        }
        "combine" => {
            let out_path = args.get(2).map(String::as_str).unwrap_or("experiments/connectivity-sweep-results-rust.csv");
            let n = combine_json_to_csv(out_path).unwrap();
            println!("Combined {n} result file(s) from {RESULTS_DIR}/ into {out_path}");
        }
        other => {
            eprintln!("Unknown mode \"{other}\". Use: bench | quick | full [shardIndex] [shardCount] | combine [outPath]");
            std::process::exit(1);
        }
    }
}
