// A durable, content-keyed cache of wind fields, so experiments can use real
// weather without a ~55s fetch (and a running Python backend) per run.
//
// **Why a cache file rather than a long-lived process.** The sweeps are single
// processes with rayon workers, and `World` takes `Arc<WindField>`, so one
// fetch is already shared across every thread within a run. The cost is only
// at startup, between runs — and a daemon would answer that by making every
// multi-hour `nohup` sweep depend on a second process staying alive.
//
// The stronger reason is that a cache entry is *pinned*. Every result in
// experiments/ is paired: identical inputs across configurations, so per-cell
// differences cancel the dominant variance. A daemon serving "current" wind
// would let the field drift between runs, and comparisons across runs would
// quietly stop being paired without anything looking wrong. A cache file makes
// wind a versioned input, exactly like the seed — which is what it has to be
// for a protocol comparison to mean anything.
//
// **Identity.** An entry is keyed on everything that determines the field:
// which file, the bytes of that file (size and mtime, not just its path), the
// time step within it, and the spatial stride. The path alone would not do —
// these filenames are CDS request hashes and get reused across re-downloads,
// so keying on the path would happily serve a stale field forever after a file
// was replaced. A changed .nc produces a different key, misses, and refetches.
//
// **Several fields at once is the point, not a bonus.** One ERA5 file commonly
// holds dozens of hourly steps over the same domain — different weather, same
// place, already downloaded. That makes wind a blocking factor an experiment
// can cross with the seed, so an effect can be shown to hold *across weather*
// rather than on one Tuesday. See `bin/wind_cache.rs` for enumerating them.
//
// Values are stored as f64, matching the JSON exactly rather than nearly. f32
// would halve the file and would almost certainly be fine — the backend rounds
// to a few decimals before serializing — but "almost certainly fine" is how
// you end up unable to explain a small discrepancy between a cached run and a
// live one at some later date.

use crate::wind_field::{Level, WindField, WindHeader};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// Bumped if the binary layout below changes, so an old cache is rejected
/// rather than misread.
const MAGIC: &[u8; 8] = b"ZMWIND01";

/// What `/api/wind-levels/source` reports. Everything here except `problem`
/// and `config` participates in identity.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceMeta {
    pub file: Option<String>,
    #[serde(rename = "fileSize")]
    pub file_size: Option<u64>,
    #[serde(rename = "fileMtime")]
    pub file_mtime: Option<i64>,
    #[serde(rename = "timeIndex")]
    pub time_index: u32,
    #[serde(rename = "validTime")]
    pub valid_time: Option<String>,
    #[serde(rename = "timeStepCount")]
    pub time_step_count: u32,
    #[serde(rename = "spatialStride")]
    pub spatial_stride: Option<u32>,
    pub synthetic: bool,
    #[serde(default)]
    pub problem: Option<String>,
}

impl SourceMeta {
    /// A short, stable key over exactly the things that change the field.
    ///
    /// Synthetic wind is keyed separately and without file identity, since it
    /// is generated rather than read — it is still worth caching, because it
    /// costs the same ~46s build.
    pub fn cache_key(&self) -> String {
        let identity = if self.synthetic {
            "synthetic".to_string()
        } else {
            format!(
                "{}|{}|{}|{}|{}",
                self.file.as_deref().unwrap_or(""),
                self.file_size.unwrap_or(0),
                self.file_mtime.unwrap_or(0),
                self.time_index,
                self.spatial_stride.unwrap_or(0),
            )
        };
        format!("{:016x}", fnv1a(identity.as_bytes()))
    }

    /// How this entry is named on the command line and in write-ups. Prefers
    /// the observation time, which is the thing a reader actually cares about
    /// — "2024-01-15T06:00:00" says more than a hash does.
    pub fn label(&self) -> String {
        if self.synthetic {
            return "synthetic".to_string();
        }
        match &self.valid_time {
            Some(t) => t.clone(),
            None => format!("step{}", self.time_index),
        }
    }
}

/// Not cryptographic and doesn't need to be: this names cache entries, it
/// doesn't defend them. FNV-1a is stable across platforms and releases, which
/// a `DefaultHasher` explicitly is not — that one is allowed to change between
/// Rust versions, which would orphan every existing entry on a toolchain
/// upgrade.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// One cached field, as recorded in its sidecar manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Entry {
    pub key: String,
    pub label: String,
    pub source: SourceMeta,
    /// When this entry was written, so a stale cache is at least diagnosable.
    pub cached_at: String,
    pub bytes: u64,
    pub levels: usize,
    pub nx: usize,
    pub ny: usize,
}

pub struct WindCache {
    dir: PathBuf,
}

impl WindCache {
    /// Defaults to `$ZM_WIND_CACHE`, else `sim-server/.wind-cache`. An env var
    /// because a cache of several hundred MB per entry is exactly the kind of
    /// thing someone wants on a different disk.
    pub fn default_dir() -> PathBuf {
        match std::env::var_os("ZM_WIND_CACHE") {
            Some(p) => PathBuf::from(p),
            None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".wind-cache"),
        }
    }

    pub fn open() -> std::io::Result<Self> {
        Self::at(Self::default_dir())
    }

    pub fn at(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(WindCache { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn field_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.bin"))
    }

    fn manifest_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    pub fn contains(&self, key: &str) -> bool {
        self.field_path(key).exists() && self.manifest_path(key).exists()
    }

    /// Every entry present, newest observation first.
    pub fn entries(&self) -> std::io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        for e in fs::read_dir(&self.dir)? {
            let path = e?.path();
            if path.extension().is_some_and(|x| x == "json") {
                let text = fs::read_to_string(&path)?;
                if let Ok(entry) = serde_json::from_str::<Entry>(&text) {
                    // A manifest without its field is a half-written entry;
                    // report only what can actually be loaded.
                    if self.field_path(&entry.key).exists() {
                        out.push(entry);
                    }
                }
            }
        }
        out.sort_by(|a, b| b.label.cmp(&a.label));
        Ok(out)
    }

    /// Resolve a user-supplied name: a key, a label (`2024-01-15T06:00:00`),
    /// or a unique prefix of either.
    pub fn find(&self, name: &str) -> std::io::Result<Option<Entry>> {
        let entries = self.entries()?;
        if let Some(e) = entries.iter().find(|e| e.key == name || e.label == name) {
            return Ok(Some(e.clone()));
        }
        let mut hits = entries
            .iter()
            .filter(|e| e.key.starts_with(name) || e.label.starts_with(name));
        match (hits.next(), hits.next()) {
            (Some(e), None) => Ok(Some(e.clone())),
            _ => Ok(None),
        }
    }

    pub fn store(&self, source: &SourceMeta, field: &WindField) -> std::io::Result<Entry> {
        let key = source.cache_key();
        // Write the field first and the manifest second: `entries()` only
        // reports an entry once both exist, so an interrupted store is
        // invisible rather than corrupt.
        let path = self.field_path(&key);
        let tmp = path.with_extension("bin.partial");
        write_field(&tmp, field)?;
        fs::rename(&tmp, &path)?;

        let entry = Entry {
            key: key.clone(),
            label: source.label(),
            source: source.clone(),
            cached_at: now_iso8601(),
            bytes: fs::metadata(&path)?.len(),
            levels: field.levels.len(),
            nx: field.header.nx,
            ny: field.header.ny,
        };
        fs::write(self.manifest_path(&key), serde_json::to_vec_pretty(&entry)?)?;
        Ok(entry)
    }

    pub fn load(&self, key: &str) -> std::io::Result<WindField> {
        read_field(&self.field_path(key))
    }
}

fn now_iso8601() -> String {
    // Seconds since epoch is enough to date an entry, and avoids a chrono
    // dependency for one string.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch:{secs}")
}

fn write_field(path: &Path, f: &WindField) -> std::io::Result<()> {
    let file = fs::File::create(path)?;
    let mut w = BufWriter::with_capacity(1 << 20, file);
    w.write_all(MAGIC)?;
    let h = &f.header;
    w.write_all(&(h.nx as u32).to_le_bytes())?;
    w.write_all(&(h.ny as u32).to_le_bytes())?;
    for v in [h.lo1, h.la1, h.lo2, h.la2, h.dx, h.dy] {
        w.write_all(&v.to_le_bytes())?;
    }
    w.write_all(&(f.levels.len() as u32).to_le_bytes())?;
    for lvl in &f.levels {
        w.write_all(&lvl.pressure_hpa.to_le_bytes())?;
        w.write_all(&lvl.altitude_m.to_le_bytes())?;
        for grid in [&lvl.u_data, &lvl.v_data] {
            if grid.len() != h.ny || grid.iter().any(|r| r.len() != h.nx) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "level grid does not match the header's nx/ny",
                ));
            }
            for row in grid {
                for &val in row {
                    w.write_all(&val.to_le_bytes())?;
                }
            }
        }
    }
    w.flush()
}

fn read_field(path: &Path) -> std::io::Result<WindField> {
    let file = fs::File::open(path)?;
    let mut r = BufReader::with_capacity(1 << 20, file);

    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is not a wind cache file of this version (delete it and refetch)",
                path.display()
            ),
        ));
    }

    let nx = read_u32(&mut r)? as usize;
    let ny = read_u32(&mut r)? as usize;
    let mut hv = [0f64; 6];
    for slot in hv.iter_mut() {
        *slot = read_f64(&mut r)?;
    }
    let header = WindHeader {
        nx,
        ny,
        lo1: hv[0],
        la1: hv[1],
        lo2: hv[2],
        la2: hv[3],
        dx: hv[4],
        dy: hv[5],
    };

    let n_levels = read_u32(&mut r)? as usize;
    let mut levels = Vec::with_capacity(n_levels);
    for _ in 0..n_levels {
        let pressure_hpa = read_f64(&mut r)?;
        let altitude_m = read_f64(&mut r)?;
        let mut grids = Vec::with_capacity(2);
        for _ in 0..2 {
            let mut grid = Vec::with_capacity(ny);
            for _ in 0..ny {
                let mut row = vec![0f64; nx];
                let mut buf = vec![0u8; nx * 8];
                r.read_exact(&mut buf)?;
                for (i, slot) in row.iter_mut().enumerate() {
                    *slot = f64::from_le_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap());
                }
                grid.push(row);
            }
            grids.push(grid);
        }
        let v_data = grids.pop().unwrap();
        let u_data = grids.pop().unwrap();
        levels.push(Level { pressure_hpa, altitude_m, u_data, v_data });
    }
    Ok(WindField { header, levels })
}

fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_f64(r: &mut impl Read) -> std::io::Result<f64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(f64::from_le_bytes(b))
}

/// Resolve the `--wind` argument every experiment harness takes.
///
/// `"none"` (or empty) is zero wind — the frozen-topology baseline every
/// existing result in experiments/ was measured under, and still the default,
/// so nothing silently re-baselines. Anything else names a cache entry.
///
/// Deliberately fails rather than falling back to zero wind on a miss. A run
/// that quietly used a different field than the one asked for is worse than a
/// run that didn't happen, because its numbers look fine.
pub fn resolve(name: &str) -> Result<WindField, String> {
    if name.is_empty() || name == "none" || name == "zero" {
        return Ok(WindField::zero());
    }
    let cache = WindCache::open().map_err(|e| format!("opening wind cache: {e}"))?;
    let entry = cache
        .find(name)
        .map_err(|e| format!("reading wind cache: {e}"))?
        .ok_or_else(|| {
            let known = cache
                .entries()
                .map(|v| v.iter().map(|e| e.label.clone()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            let known = if known.is_empty() { "(cache is empty)".into() } else { known };
            format!(
                "no wind cache entry matching {name:?} in {}.\n  known: {known}\n  \
                 populate it with: cargo run --release --bin wind_cache -- fetch <timeIndex...>",
                cache.dir().display()
            )
        })?;
    cache.load(&entry.key).map_err(|e| format!("loading wind entry {}: {e}", entry.key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> SourceMeta {
        SourceMeta {
            file: Some("data/abc.nc".into()),
            file_size: Some(12345),
            file_mtime: Some(999),
            time_index: 3,
            valid_time: Some("2024-01-15T06:00:00".into()),
            time_step_count: 24,
            spatial_stride: Some(2),
            synthetic: false,
            problem: None,
        }
    }

    fn small_field() -> WindField {
        WindField {
            header: WindHeader {
                nx: 3,
                ny: 2,
                lo1: 0.0,
                la1: 10.0,
                lo2: 20.0,
                la2: 0.0,
                dx: 10.0,
                dy: 10.0,
            },
            levels: vec![Level {
                pressure_hpa: 250.0,
                altitude_m: 10000.0,
                u_data: vec![vec![1.5, -2.25, 3.0], vec![0.0, 4.75, -6.5]],
                v_data: vec![vec![-1.0, 2.0, 0.5], vec![7.25, -8.0, 9.125]],
            }],
        }
    }

    /// The whole point of the format: what comes back is what went in, so a
    /// cached run and a live-fetched one cannot diverge in the last bits.
    #[test]
    fn a_field_round_trips_exactly() {
        let dir = std::env::temp_dir().join(format!("zm-wind-rt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let cache = WindCache::at(&dir).unwrap();
        let field = small_field();
        let entry = cache.store(&meta(), &field).unwrap();

        let back = cache.load(&entry.key).unwrap();
        assert_eq!(back.header.nx, field.header.nx);
        assert_eq!(back.header.ny, field.header.ny);
        assert_eq!(back.levels.len(), 1);
        assert_eq!(back.levels[0].u_data, field.levels[0].u_data);
        assert_eq!(back.levels[0].v_data, field.levels[0].v_data);
        assert_eq!(back.levels[0].altitude_m, field.levels[0].altitude_m);
        // And sampling agrees, which is what the simulation actually does.
        assert_eq!(back.sample(5.0, 5.0, 10000.0), field.sample(5.0, 5.0, 10000.0));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Replacing the .nc under a cached entry must not keep serving the old
    /// field. Keying on the path alone would do exactly that, and these
    /// filenames are request hashes that get reused across re-downloads.
    #[test]
    fn changing_the_source_file_changes_the_key() {
        let base = meta();
        let mut touched = base.clone();
        touched.file_mtime = Some(1000);
        let mut regrown = base.clone();
        regrown.file_size = Some(999_999);

        assert_ne!(base.cache_key(), touched.cache_key(), "a rewritten file must miss");
        assert_ne!(base.cache_key(), regrown.cache_key(), "a resized file must miss");
        assert_eq!(base.cache_key(), meta().cache_key(), "and the same file must hit");
    }

    /// Time steps within one file are distinct fields — that is what makes
    /// wind usable as a blocking factor without downloading anything more.
    #[test]
    fn each_time_step_is_its_own_entry() {
        let a = meta();
        let mut b = meta();
        b.time_index = 4;
        b.valid_time = Some("2024-01-15T07:00:00".into());
        assert_ne!(a.cache_key(), b.cache_key());
        assert_ne!(a.label(), b.label());
    }

    /// Stride changes the grid, so it changes the field.
    #[test]
    fn stride_participates_in_identity() {
        let a = meta();
        let mut b = meta();
        b.spatial_stride = Some(8);
        assert_ne!(a.cache_key(), b.cache_key());
    }

    /// A miss must be loud. Silently falling back to zero wind would produce a
    /// run whose numbers look entirely reasonable and mean something else.
    #[test]
    fn an_unknown_name_is_an_error_not_a_fallback() {
        let dir = std::env::temp_dir().join(format!("zm-wind-miss-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        std::env::set_var("ZM_WIND_CACHE", &dir);
        let err = resolve("nope-not-here").unwrap_err();
        assert!(err.contains("no wind cache entry"), "unhelpful error: {err}");
        assert!(err.contains("wind_cache"), "should say how to populate it: {err}");
        // ...but zero wind stays available by name, since it is the baseline.
        assert!(resolve("none").is_ok());
        std::env::remove_var("ZM_WIND_CACHE");
        let _ = fs::remove_dir_all(&dir);
    }
}
