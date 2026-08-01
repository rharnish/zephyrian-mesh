// Populates and inspects the wind cache that experiments read from.
//
//   cargo run --release --bin wind_cache -- steps          what's in the .nc
//   cargo run --release --bin wind_cache -- fetch 0 6 12   cache those steps
//   cargo run --release --bin wind_cache -- list           what's cached
//
// This is the only thing that talks to wind_backend.py. Once a step is cached
// the Python side can be stopped: experiments load from disk in milliseconds
// and never touch the network, which is what makes a multi-hour sweep
// reproducible months later.
//
// Fetching is deliberately slow — the backend rebuilds the full grid for any
// step other than its configured default, ~46s apiece — because it happens
// once per field, ever. `steps` is the cheap counterpart: it reads only
// metadata, so you can see what a file holds before committing to fetching
// anything.

use sim_server::config::WIND_API_URL;
use sim_server::wind_cache::{SourceMeta, WindCache};
use sim_server::wind_field::WindField;

fn source_url() -> String {
    format!("{WIND_API_URL}/source")
}

fn fetch_source(time_index: Option<u32>) -> Result<SourceMeta, String> {
    let url = match time_index {
        Some(i) => format!("{}?timeIndex={i}", source_url()),
        None => source_url(),
    };
    let resp = reqwest::blocking::get(&url)
        .map_err(|e| format!("GET {url} failed (is wind_backend.py running?): {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url} -> {}: {}", resp.status(), resp.text().unwrap_or_default()));
    }
    resp.json::<SourceMeta>().map_err(|e| format!("decoding {url}: {e}"))
}

/// `default_step` skips the query parameter, so the server answers from the
/// bytes it built at startup instead of rebuilding the grid — ~46s saved on
/// the overwhelmingly common case of caching the configured step.
fn fetch_field(time_index: u32, default_step: bool) -> Result<WindField, String> {
    let url = if default_step {
        WIND_API_URL.to_string()
    } else {
        format!("{WIND_API_URL}?timeIndex={time_index}")
    };
    let client = reqwest::blocking::Client::builder()
        // The payload is ~350MB and the server rebuilds the grid for a
        // non-default step; the default 30s timeout expires long before it
        // answers.
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .map_err(|e| format!("building http client: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("GET {url} failed (is wind_backend.py running?): {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url} -> {}: {}", resp.status(), resp.text().unwrap_or_default()));
    }
    resp.json::<WindField>().map_err(|e| format!("decoding wind payload: {e}"))
}

fn cmd_steps() -> Result<(), String> {
    let base = fetch_source(None)?;
    if base.synthetic {
        println!("Serving SYNTHETIC wind: {}", base.problem.unwrap_or_default());
        println!("Nothing to enumerate; `fetch` will cache the synthetic field as \"synthetic\".");
        return Ok(());
    }
    println!(
        "file    {}\nstride  {}\nsteps   {}\n",
        base.file.clone().unwrap_or_default(),
        base.spatial_stride.unwrap_or(0),
        base.time_step_count
    );
    let cache = WindCache::open().map_err(|e| format!("opening cache: {e}"))?;
    println!("{:>5}  {:<21}  {}", "index", "validTime", "cached");
    println!("{}", "-".repeat(40));
    for i in 0..base.time_step_count {
        let m = fetch_source(Some(i))?;
        let mark = if cache.contains(&m.cache_key()) { "yes" } else { "" };
        println!("{:>5}  {:<21}  {}", i, m.label(), mark);
    }
    Ok(())
}

fn cmd_list() -> Result<(), String> {
    let cache = WindCache::open().map_err(|e| format!("opening cache: {e}"))?;
    let entries = cache.entries().map_err(|e| format!("reading cache: {e}"))?;
    println!("cache: {}", cache.dir().display());
    if entries.is_empty() {
        println!("(empty — populate with `wind_cache fetch <timeIndex...>`)");
        return Ok(());
    }
    let total: u64 = entries.iter().map(|e| e.bytes).sum();
    println!("{:<21}  {:<16}  {:>7}  {:>5}  {}", "label", "key", "MB", "lvls", "grid");
    println!("{}", "-".repeat(66));
    for e in &entries {
        println!(
            "{:<21}  {:<16}  {:>7.1}  {:>5}  {}x{}",
            e.label,
            e.key,
            e.bytes as f64 / 1e6,
            e.levels,
            e.nx,
            e.ny
        );
    }
    println!("\n{} entries, {:.1} MB total", entries.len(), total as f64 / 1e6);
    Ok(())
}

fn cmd_fetch(indices: &[u32]) -> Result<(), String> {
    let cache = WindCache::open().map_err(|e| format!("opening cache: {e}"))?;
    let base = fetch_source(None)?;

    if base.synthetic {
        if cache.contains(&base.cache_key()) {
            println!("synthetic field already cached");
            return Ok(());
        }
        println!("fetching synthetic field...");
        let field = fetch_field(0, true)?;
        let e = cache.store(&base, &field).map_err(|x| format!("storing: {x}"))?;
        println!("cached {} ({:.1} MB)", e.label, e.bytes as f64 / 1e6);
        return Ok(());
    }

    let indices: Vec<u32> =
        if indices.is_empty() { vec![base.time_index] } else { indices.to_vec() };

    for &i in &indices {
        let meta = fetch_source(Some(i))?;
        let key = meta.cache_key();
        if cache.contains(&key) {
            println!("step {i} ({}) already cached, skipping", meta.label());
            continue;
        }
        println!("step {i} ({}): fetching, ~1 min...", meta.label());
        let started = std::time::Instant::now();
        let field = fetch_field(i, i == base.time_index)?;
        let e = cache.store(&meta, &field).map_err(|x| format!("storing: {x}"))?;
        println!(
            "  cached {} — {:.1} MB, {} levels, {}x{}, {:.0}s",
            e.label,
            e.bytes as f64 / 1e6,
            e.levels,
            e.nx,
            e.ny,
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn usage() -> String {
    "wind_cache — populate and inspect the wind field cache experiments read from

  steps                 list the time steps in the configured .nc, and which
                        are already cached (metadata only, fast)
  fetch [index...]      fetch and cache those steps; no index means the one
                        wind_source.json selects. Already-cached steps are
                        skipped, so re-running is safe.
  list                  what is currently cached

Requires wind_backend.py running for `steps` and `fetch`; `list` does not.
Cache location: $ZM_WIND_CACHE, else sim-server/.wind-cache

Experiments then take --wind <label|key>, e.g.
  protocol_compare --wind 2024-01-15T06:00:00
with --wind none (the default) meaning zero wind."
        .to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(|s| s.as_str()) {
        Some("steps") => cmd_steps(),
        Some("list") => cmd_list(),
        Some("fetch") => {
            let mut idx = Vec::new();
            for a in &args[1..] {
                match a.parse::<u32>() {
                    Ok(v) => idx.push(v),
                    Err(_) => {
                        eprintln!("not a time index: {a:?}");
                        std::process::exit(2);
                    }
                }
            }
            cmd_fetch(&idx)
        }
        Some("-h") | Some("--help") | None => {
            println!("{}", usage());
            return;
        }
        Some(other) => {
            eprintln!("unknown command {other:?}\n\n{}", usage());
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
