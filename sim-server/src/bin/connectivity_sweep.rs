// Rust port of experiments/connectivity-sweep.mjs — same model, same output
// format (JSON per combo + combined CSV), so results are directly
// comparable to the JS run already checked into experiments/results/ and
// experiments/connectivity-sweep-results.csv. Reuses sim-server's
// already-ported physics/link-detection modules as a library (see ../lib.rs)
// rather than reimplementing them.
//
// Writes to SEPARATE output paths by default (experiments/results-rust/,
// experiments/connectivity-sweep-results-rust.csv) so the existing JS
// results aren't touched — this run exists to compare wall-clock time and
// (ideally) confirm matching output, not to replace the JS data.
//
// The sweep grid itself lives in a JSON config file (default:
// experiments/sweep-config.json — see SweepConfig below), not in code, so
// running a new experiment doesn't need a rebuild. `full` parallelizes
// across combos in-process with rayon (one wind fetch, one process to
// profile) and auto-combines into the CSV when done — this replaced the
// old shard-script model (see run-shards-rust.sh in git history) where
// N separate processes each fetched wind and had to be launched/waited-on
// externally.
//
// Usage (run from the sim-server/ directory, or repo root — paths below
// are relative to cwd, matching connectivity-sweep.mjs's convention of
// being run from the repo root):
//   cargo run --release --bin connectivity_sweep -- bench
//   cargo run --release --bin connectivity_sweep -- quick
//   cargo run --release --bin connectivity_sweep -- full [configPath] [outCsvPath]
//   cargo run --release --bin connectivity_sweep -- combine [resultsDir] [outPath]
//
// Uses real wind data fetched once at startup from wind_backend.py (same
// endpoint sim-server's main binary uses), falling back to zero wind if
// that fetch fails — matching main.rs's fetch_wind_field behavior. Balloons
// still hold their spawn (lon, lat) *only* under the zero-wind fallback;
// with real wind loaded, they drift laterally like they do in the live app.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sim_server::balloon::Balloon;
use sim_server::config::{BALLOON_MAX_ALT, BALLOON_MIN_ALT, GRID_CELL_SIZE_DEG, INITIAL_TOWERS, WIND_API_URL};
use sim_server::geo::{precompute, random_global_position, Precomputed};
use sim_server::link_detection::ConnectivityScratch;
use sim_server::spatial_grid::SpatialGrid;
use sim_server::tower::Tower;
use sim_server::wind_field::WindField;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const PAYLOAD_INTERVAL_SEC: i64 = 5 * 60; // fixed, not swept

const DEFAULT_CONFIG_PATH: &str = "experiments/sweep-config.json";
const DEFAULT_RESULTS_DIR: &str = "experiments/results-rust";
const DEFAULT_OUT_CSV: &str = "experiments/connectivity-sweep-results-rust.csv";
const DEFAULT_ACK_DURATION_SEC: i64 = 30;
const DEFAULT_DURATION_HOURS: f64 = 24.0;

fn default_ack_duration_sec() -> i64 {
    DEFAULT_ACK_DURATION_SEC
}
fn default_duration_hours() -> f64 {
    DEFAULT_DURATION_HOURS
}
fn default_results_dir() -> String {
    DEFAULT_RESULTS_DIR.to_string()
}
fn default_out_csv() -> String {
    DEFAULT_OUT_CSV.to_string()
}

// The sweep grid, loaded from JSON (default path: experiments/sweep-config.json)
// instead of being hardcoded, so a new experiment is "edit the file, re-run"
// rather than "edit the source, rebuild". All fields but the three grid arrays
// are optional and fall back to the defaults above.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SweepConfig {
    horizon_coeffs: Vec<f64>,
    n_balloons: Vec<u32>,
    fallback_timeout_min: Vec<i64>,
    #[serde(default = "default_ack_duration_sec")]
    ack_duration_sec: i64,
    #[serde(default = "default_duration_hours")]
    duration_hours: f64,
    #[serde(default = "default_results_dir")]
    results_dir: String,
    #[serde(default = "default_out_csv")]
    out_csv: String,
}

fn load_config(path: &str) -> SweepConfig {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read sweep config {path}: {e}"));
    serde_json::from_str(&content).unwrap_or_else(|e| panic!("failed to parse sweep config {path}: {e}"))
}

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

fn combos(config: &SweepConfig) -> Vec<Combo> {
    let mut out = Vec::new();
    for &horizon_coeff in &config.horizon_coeffs {
        for &n_balloons in &config.n_balloons {
            for &fallback_timeout_min in &config.fallback_timeout_min {
                out.push(Combo { horizon_coeff, n_balloons, fallback_timeout_min, ack_duration_sec: config.ack_duration_sec });
            }
        }
    }
    out
}

fn combo_file_name(combo: &Combo) -> String {
    let h = format!("{:.2}", combo.horizon_coeff).replace('.', "p");
    format!("h{h}_n{}_t{}_a{}.json", combo.n_balloons, combo.fallback_timeout_min, combo.ack_duration_sec)
}

fn write_combo_json(results_dir: &str, combo: &Combo, row: &ResultRow) -> std::io::Result<String> {
    std::fs::create_dir_all(results_dir)?;
    let path = format!("{results_dir}/{}", combo_file_name(combo));
    std::fs::write(&path, serde_json::to_string_pretty(row).unwrap())?;
    Ok(path)
}

fn combine_json_to_csv(results_dir: &str, out_path: &str) -> std::io::Result<usize> {
    let mut rows: Vec<ResultRow> = Vec::new();
    let mut combo_paths: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::fs::read_dir(results_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path)?;
            match serde_json::from_str(&content) {
                Ok(row) => {
                    rows.push(row);
                    combo_paths.push(path);
                }
                // Non-combo JSON can land in RESULTS_DIR too (e.g. chart-data.json
                // written by summarize_sweep.py) — skip rather than panic on it.
                Err(_) => eprintln!("skipping non-combo-result JSON file: {}", path.display()),
            }
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
    archive_combo_jsons(results_dir, &combo_paths)?;
    Ok(rows.len())
}

// Once a combo's JSON has been folded into the combined CSV, it's only
// useful for debugging a specific cell or re-combining — not for browsing
// resultsDir, which otherwise fills up with hundreds of tiny per-combo
// files. Move them out of the way into a `json/` subfolder rather than
// deleting them, since `combine` can be re-run against archived files and
// nothing else here depends on them being gone.
fn archive_combo_jsons(results_dir: &str, combo_paths: &[std::path::PathBuf]) -> std::io::Result<()> {
    if combo_paths.is_empty() {
        return Ok(());
    }
    let archive_dir = format!("{results_dir}/json");
    std::fs::create_dir_all(&archive_dir)?;
    for path in combo_paths {
        if let Some(file_name) = path.file_name() {
            std::fs::rename(path, std::path::Path::new(&archive_dir).join(file_name))?;
        }
    }
    Ok(())
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
                n_balloons: 1600, // worst case across every sweep run so far
                fallback_timeout_min: 30,
                ack_duration_sec: DEFAULT_ACK_DURATION_SEC,
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
        }
        "quick" => {
            let wind = fetch_wind_field_blocking();
            let duration_sec = 2 * 60 * 60; // 2 sim hours
            println!("{:<10} {:<12} {:<14} {:<10} {:<12}", "nBalloons", "horizonCoeff", "totalPayloads", "pctRadio", "pctSatellite");
            for &n_balloons in &[50, 200] {
                for &horizon_coeff in &[3.4, 4.0] {
                    let combo = Combo { horizon_coeff, n_balloons, fallback_timeout_min: 20, ack_duration_sec: DEFAULT_ACK_DURATION_SEC };
                    let row = run_combo(&combo, &wind, duration_sec);
                    println!(
                        "{:<10} {:<12} {:<14} {:<10.1} {:<12.1}",
                        row.n_balloons, row.horizon_coeff, row.total_payloads, row.pct_radio, row.pct_satellite
                    );
                }
            }
        }
        "full" => {
            let config_path = args.get(2).map(String::as_str).unwrap_or(DEFAULT_CONFIG_PATH);
            let config = load_config(config_path);
            let out_csv = args.get(3).map(String::as_str).unwrap_or(config.out_csv.as_str()).to_string();
            let results_dir = config.results_dir.clone();
            let duration_sec = (config.duration_hours * 3600.0) as i64;

            let wind = fetch_wind_field_blocking();
            let all = combos(&config);
            println!(
                "Loaded {config_path} — running {} combination(s) x {} sim hours each across {} thread(s).",
                all.len(),
                config.duration_hours,
                rayon::current_num_threads()
            );
            std::fs::create_dir_all(&results_dir).unwrap();

            let t0 = Instant::now();
            let done = AtomicUsize::new(0);
            let total = all.len();
            all.par_iter().for_each(|combo| {
                let path = format!("{results_dir}/{}", combo_file_name(combo));
                // Completed combos get archived into results_dir/json/ by
                // combine_json_to_csv, so check there too — otherwise a
                // resumed/extended run would silently recompute everything
                // from a prior completed run in the same resultsDir.
                let archived_path = format!("{results_dir}/json/{}", combo_file_name(combo));
                if std::path::Path::new(&path).exists() || std::path::Path::new(&archived_path).exists() {
                    println!("[skip] already done: {path}");
                    return;
                }
                let row = run_combo(combo, &wind, duration_sec);
                write_combo_json(&results_dir, combo, &row).unwrap();
                let n_done = done.fetch_add(1, Ordering::Relaxed) + 1;
                let elapsed = t0.elapsed().as_secs_f64();
                let rate = n_done as f64 / elapsed;
                let eta_sec = if rate > 0.0 { (total - n_done) as f64 / rate } else { 0.0 };
                println!(
                    "[{n_done}/{total}] wrote {path} — elapsed {:.0}s, ETA {:.1} min",
                    elapsed,
                    eta_sec / 60.0
                );
            });

            println!("Sweep done, combining results into {out_csv}");
            let n = combine_json_to_csv(&results_dir, &out_csv).unwrap();
            println!("Combined {n} result file(s) from {results_dir}/ into {out_csv}");
        }
        "combine" => {
            let results_dir = args.get(2).map(String::as_str).unwrap_or(DEFAULT_RESULTS_DIR);
            let out_path = args.get(3).map(String::as_str).unwrap_or(DEFAULT_OUT_CSV);
            let n = combine_json_to_csv(results_dir, out_path).unwrap();
            println!("Combined {n} result file(s) from {results_dir}/ into {out_path}");
        }
        other => {
            eprintln!(
                "Unknown mode \"{other}\". Use: bench | quick | full [configPath] [outCsvPath] | combine [resultsDir] [outPath]"
            );
            std::process::exit(1);
        }
    }
}
