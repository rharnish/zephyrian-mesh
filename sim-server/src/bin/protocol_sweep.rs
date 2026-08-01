// Same sweep infrastructure as connectivity_sweep.rs (JSON-config-driven grid,
// resumable per-combo JSON, rayon-parallel, combined CSV, real wind) — but
// driving the *real* C1+C2 protocol (sim_server::sim::World, beacon::step +
// bundle::step) instead of the toy accounting model connectivity_sweep.rs
// measures. bundle_delivery.rs already proves the real protocol works, as a
// single-run, zero-wind, offline diagnostic; this is that same real protocol,
// swept across horizon coefficient x balloon count with real wind, reporting
// both omniscient ground-truth connectivity (mean_degree, grounded_pct, from
// union-find) and what the decentralized protocol — which never gets to see
// that truth — actually achieves. The gap between the two is the point.
//
// The toy model's other two axes (fallback_timeout_min, ack_duration_sec)
// have no real-protocol analogue — that timing is governed by fixed
// config.rs constants (BUNDLE_MAX_AGE_ROUNDS, BEACON_INTERVAL_ROUNDS, ...),
// not per-run parameters, so they're dropped rather than faked.
//
// Usage (run from the sim-server/ directory, or repo root):
//   cargo run --release --bin protocol_sweep -- bench
//   cargo run --release --bin protocol_sweep -- quick
//   cargo run --release --bin protocol_sweep -- full [configPath] [outCsvPath]
//   cargo run --release --bin protocol_sweep -- combine [resultsDir] [outPath]

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sim_server::protocol::dv_dtn::bundle::BundleStats;
use sim_server::config::*;
use sim_server::sim::World;
use sim_server::wind_field::WindField;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

const DEFAULT_CONFIG_PATH: &str = "experiments/protocol-sweep-config.json";
const DEFAULT_RESULTS_DIR: &str = "experiments/protocol-results";
const DEFAULT_OUT_CSV: &str = "experiments/protocol-sweep-results.csv";
const DEFAULT_DURATION_HOURS: f64 = 24.0;

fn default_duration_hours() -> f64 {
    DEFAULT_DURATION_HOURS
}
fn default_results_dir() -> String {
    DEFAULT_RESULTS_DIR.to_string()
}
fn default_out_csv() -> String {
    DEFAULT_OUT_CSV.to_string()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SweepConfig {
    horizon_coeffs: Vec<f64>,
    n_balloons: Vec<u32>,
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
}

// Deterministic per-combo seed, same rationale as connectivity_sweep.rs: each
// combo gets its own balloon layout/altitude/duty-cycle draw, but repeated
// runs of this binary reproduce it rather than drawing fresh entropy.
fn seed_for_combo(combo: &Combo) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mix = |h: u64, x: u64| (h ^ x).wrapping_mul(FNV_PRIME);
    let mut h = FNV_OFFSET;
    h = mix(h, combo.horizon_coeff.to_bits());
    h = mix(h, combo.n_balloons as u64);
    h
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResultRow {
    horizon_coeff: f64,
    n_balloons: u32,

    // --- Omniscient truth (union-find), never seen by any balloon ----------
    mean_degree: f64,
    grounded_pct: f64,
    /// What fraction currently *believe* they have a route — a third point
    /// between ground truth and what actually got delivered.
    believed_grounded_pct: f64,

    // --- What the real decentralized protocol actually achieved ------------
    originated: u64,
    delivered: u64,
    satellite: u64,
    acked_count: u64,
    ack_lost_count: u64,
    dropped_loop: u64,
    dropped_ttl: u64,
    /// delivered / resolved — see BundleStats::completion_rate.
    completion_rate: f64,
    mean_tower_adjacent: f64,
    /// The last-hop ceiling (BundleStats::delivery_capacity_per_round) — at a
    /// glance, is this combo mesh-bound or ground-link-bound? See §4 of
    /// docs/design/MESH_COMMS_DESIGN.md.
    delivery_ceiling_per_round: f64,
}

fn advance_round(world: &mut World) -> sim_server::sim::Snapshot {
    let dt = TICK_DT_SECONDS * TIME_SCALE;
    let mut s = world.tick(dt);
    for _ in 1..COMMS_EVERY_N_TICKS {
        s = world.tick(dt);
    }
    s
}

fn run_combo(combo: &Combo, wind: &Arc<WindField>, rounds: u64) -> ResultRow {
    let mut world = World::new(wind.clone()).with_seed(seed_for_combo(combo));
    for &(lon, lat, height_m) in INITIAL_TOWERS {
        world.add_tower(lon, lat, height_m);
    }
    world.spawn_balloon_pool(combo.n_balloons);
    world.set_visible_count(combo.n_balloons);
    world.horizon_refraction_coeff = combo.horizon_coeff;

    let mut last = advance_round(&mut world);
    for _ in 1..rounds {
        last = advance_round(&mut world);
    }
    let st: BundleStats = world.bundle_stats();

    ResultRow {
        horizon_coeff: combo.horizon_coeff,
        n_balloons: combo.n_balloons,
        mean_degree: last.mean_degree,
        grounded_pct: last.grounded_pct,
        believed_grounded_pct: last.believed_grounded_pct,
        originated: st.originated,
        delivered: st.delivered,
        satellite: st.satellite,
        acked_count: st.acked,
        ack_lost_count: st.ack_lost,
        dropped_loop: st.dropped_loop,
        dropped_ttl: st.dropped_ttl,
        completion_rate: st.completion_rate(),
        mean_tower_adjacent: st.mean_tower_adjacent(),
        delivery_ceiling_per_round: st.delivery_capacity_per_round(),
    }
}

fn combos(config: &SweepConfig) -> Vec<Combo> {
    let mut out = Vec::new();
    for &horizon_coeff in &config.horizon_coeffs {
        for &n_balloons in &config.n_balloons {
            out.push(Combo { horizon_coeff, n_balloons });
        }
    }
    out
}

fn combo_file_name(combo: &Combo) -> String {
    let h = format!("{:.2}", combo.horizon_coeff).replace('.', "p");
    format!("h{h}_n{}.json", combo.n_balloons)
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
                Err(_) => eprintln!("skipping non-combo-result JSON file: {}", path.display()),
            }
        }
    }
    rows.sort_by(|a, b| {
        a.horizon_coeff.partial_cmp(&b.horizon_coeff).unwrap().then(a.n_balloons.cmp(&b.n_balloons))
    });
    let mut out = String::from(
        "horizonCoeff,nBalloons,meanDegree,groundedPct,believedGroundedPct,originated,delivered,\
         satellite,ackedCount,ackLostCount,droppedLoop,droppedTtl,completionRate,\
         meanTowerAdjacent,deliveryCeilingPerRound\n",
    );
    for r in &rows {
        out.push_str(&format!(
            "{},{},{:.3},{:.3},{:.3},{},{},{},{},{},{},{},{:.4},{:.3},{:.3}\n",
            r.horizon_coeff,
            r.n_balloons,
            r.mean_degree,
            r.grounded_pct,
            r.believed_grounded_pct,
            r.originated,
            r.delivered,
            r.satellite,
            r.acked_count,
            r.ack_lost_count,
            r.dropped_loop,
            r.dropped_ttl,
            r.completion_rate,
            r.mean_tower_adjacent,
            r.delivery_ceiling_per_round,
        ));
    }
    std::fs::write(out_path, &out)?;
    archive_combo_jsons(results_dir, &combo_paths)?;
    Ok(rows.len())
}

// Same reasoning as connectivity_sweep.rs: once folded into the CSV, per-combo
// JSON is only useful for debugging one cell or re-combining, so it's moved
// aside rather than left to clutter resultsDir or deleted outright.
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

// Same as connectivity_sweep.rs's fetch_wind_field_blocking: blocking (this
// binary isn't tokio), generous timeout (the measured ~55s response time from
// the wind_backend_perf memory note), falls back to zero wind on failure.
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

fn rounds_for(duration_hours: f64) -> u64 {
    ((duration_hours * 3600.0) / COMMS_ROUND_SIM_SECONDS).round() as u64
}

fn print_row(row: &ResultRow) {
    println!(
        "coeff={:<5.2} n={:<5} degree={:<5.2} grounded={:<5.1}% believed={:<5.1}% \
         delivered={:<6} satellite={:<6} acked={:<6} ackLost={:<6} completion={:<5.1}% \
         ceiling={:.2}/round",
        row.horizon_coeff,
        row.n_balloons,
        row.mean_degree,
        row.grounded_pct,
        row.believed_grounded_pct,
        row.delivered,
        row.satellite,
        row.acked_count,
        row.ack_lost_count,
        100.0 * row.completion_rate,
        row.delivery_ceiling_per_round,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("bench");

    match mode {
        "bench" => {
            let wind = Arc::new(fetch_wind_field_blocking());
            let duration_hours = 1.0;
            let rounds = rounds_for(duration_hours);
            let combo = Combo { horizon_coeff: 4.12, n_balloons: 1600 };
            println!(
                "Benchmarking worst case (n={}) over {duration_hours} sim hour(s) ({rounds} rounds)...",
                combo.n_balloons
            );
            let t0 = Instant::now();
            let row = run_combo(&combo, &wind, rounds);
            let wall_sec = t0.elapsed().as_secs_f64();
            print_row(&row);
            println!(
                "Wall time: {:.1}s for {duration_hours} sim hour(s) => {:.0}s ({:.1} min) per \
                 24-sim-hour combo at worst case.",
                wall_sec,
                wall_sec * 24.0 / duration_hours,
                wall_sec * 24.0 / duration_hours / 60.0
            );
        }
        "quick" => {
            let wind = Arc::new(fetch_wind_field_blocking());
            let rounds = rounds_for(2.0);
            for &n_balloons in &[400, 1200] {
                for &horizon_coeff in &[3.4, 4.12] {
                    let combo = Combo { horizon_coeff, n_balloons };
                    let row = run_combo(&combo, &wind, rounds);
                    print_row(&row);
                }
            }
        }
        "full" => {
            let config_path = args.get(2).map(String::as_str).unwrap_or(DEFAULT_CONFIG_PATH);
            let config = load_config(config_path);
            let out_csv = args.get(3).map(String::as_str).unwrap_or(config.out_csv.as_str()).to_string();
            let results_dir = config.results_dir.clone();
            let rounds = rounds_for(config.duration_hours);

            let wind = Arc::new(fetch_wind_field_blocking());
            let all = combos(&config);
            println!(
                "Loaded {config_path} — running {} combination(s) x {} sim hours ({rounds} rounds) \
                 each across {} thread(s).",
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
                let archived_path = format!("{results_dir}/json/{}", combo_file_name(combo));
                if std::path::Path::new(&path).exists() || std::path::Path::new(&archived_path).exists() {
                    println!("[skip] already done: {path}");
                    return;
                }
                let row = run_combo(combo, &wind, rounds);
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
