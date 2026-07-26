// The single authoritative simulation loop. One task owns `World` exclusively
// (see main.rs) — no locks, mutations arrive as `Command`s over a channel and
// snapshots go out as JSON over a broadcast channel. Mirrors main.js's
// tick() (balloon motion every tick, link detection throttled to every
// LINK_UPDATE_EVERY_N_TICKS ticks).

use crate::balloon::Balloon;
use crate::config::*;
use crate::geo::{horizon_km, random_global_position};
use crate::link_detection::compute_grid_edges;
use crate::spatial_grid::SpatialGrid;
use crate::tower::Tower;
use crate::union_find::UnionFind;
use crate::wind_field::WindField;
use rand::rngs::StdRng;
use std::sync::Arc;
use rand::{Rng, SeedableRng};
use serde::Serialize;

pub enum Command {
    SetBalloonCount(u32),
    AddTower { lon: f64, lat: f64, height_m: f64 },
    RemoveTower { id: u32 },
    SetHorizonRefractionCoeff(f64),
    SetPaused(bool),
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EdgeSnapshot {
    pub pair_key: String,
    pub a: (f64, f64, f64),
    pub b: (f64, f64, f64),
    pub grounded: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub tick: u64,
    pub balloons: Vec<Balloon>,
    pub towers: Vec<Tower>,
    /// `None` on ticks where links weren't recomputed (still throttled the
    /// same way main.js throttles it) — client keeps the last edge set.
    pub edges: Option<Vec<EdgeSnapshot>>,
    /// Broadcast so every connected tab's slider stays in sync with
    /// whichever tab last changed it (server is the source of truth).
    pub horizon_refraction_coeff: f64,
    /// Whether the sim is paused. Broadcast so the pause toggle stays in
    /// sync across tabs (server is the source of truth).
    pub paused: bool,
    /// Mean number of links per visible balloon (counting tower links).
    /// Carried on every snapshot, not just link ticks, so the readout holds
    /// steady between recomputes instead of blinking.
    pub mean_degree: f64,
    /// Share (0..100) of visible balloons whose component contains a tower.
    pub grounded_pct: f64,
}

pub struct World {
    pub balloons: Vec<Balloon>,
    pub towers: Vec<Tower>,
    // Shared (Arc) with the HTTP layer, which serves this same field to the
    // browser via GET /api/wind-levels — no second copy of the large payload.
    pub wind: Arc<WindField>,
    pub horizon_refraction_coeff: f64,
    pub visible_count: usize,
    pub paused: bool,
    // Last computed mesh-health readout (see the link-recompute block in
    // tick()). Held across non-link ticks so every snapshot can carry it.
    mean_degree: f64,
    grounded_pct: f64,
    next_balloon_id: u32,
    next_tower_id: u32,
    grid: SpatialGrid,
    union_find: UnionFind,
    rng: StdRng,
    tick_count: u64,
}

impl World {
    pub fn new(wind: Arc<WindField>) -> Self {
        World {
            balloons: Vec::new(),
            towers: Vec::new(),
            wind,
            horizon_refraction_coeff: DEFAULT_HORIZON_REFRACTION_COEFF,
            visible_count: 0,
            paused: false,
            mean_degree: 0.0,
            grounded_pct: 0.0,
            next_balloon_id: 0,
            next_tower_id: 0,
            grid: SpatialGrid::new(GRID_CELL_SIZE_DEG),
            union_find: UnionFind::new(),
            rng: StdRng::from_entropy(),
            tick_count: 0,
        }
    }

    /// Spawns the full always-on pool. Call once at startup.
    pub fn spawn_balloon_pool(&mut self, n: u32) {
        self.balloons.clear();
        for _ in 0..n {
            let (lon, lat) = random_global_position(&mut self.rng);
            let alt = BALLOON_MIN_ALT + self.rng.gen_range(0.0..(BALLOON_MAX_ALT - BALLOON_MIN_ALT));
            self.balloons.push(Balloon::new(self.next_balloon_id, lon, lat, alt));
            self.next_balloon_id += 1;
        }
    }

    /// Changes how many (of the already-flying pool) are visible/connected —
    /// no respawn, no discontinuity.
    pub fn set_visible_count(&mut self, n: u32) {
        self.visible_count = (n as usize).min(self.balloons.len());
    }

    pub fn add_tower(&mut self, lon: f64, lat: f64, height_m: f64) {
        self.towers.push(Tower::new(self.next_tower_id, lon, lat, height_m));
        self.next_tower_id += 1;
    }

    pub fn remove_tower(&mut self, id: u32) {
        self.towers.retain(|t| t.id != id);
    }

    pub fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::SetBalloonCount(n) => self.set_visible_count(n),
            Command::AddTower { lon, lat, height_m } => self.add_tower(lon, lat, height_m),
            Command::RemoveTower { id } => self.remove_tower(id),
            Command::SetHorizonRefractionCoeff(c) => self.horizon_refraction_coeff = c,
            Command::SetPaused(p) => self.paused = p,
        }
    }

    /// Advance one tick. Returns a snapshot to broadcast.
    pub fn tick(&mut self, dt_seconds: f64) -> Snapshot {
        // Paused: freeze physics and skip link recompute, but still broadcast
        // current state so late-joining clients render and the pause toggle
        // stays in sync. `edges: None` means clients keep their last edge set
        // (positions aren't moving, so the frozen links stay correct).
        if self.paused {
            let visible = &self.balloons[..self.visible_count];
            return Snapshot {
                tick: self.tick_count,
                balloons: visible.to_vec(),
                towers: self.towers.clone(),
                edges: None,
                horizon_refraction_coeff: self.horizon_refraction_coeff,
                paused: true,
                mean_degree: self.mean_degree,
                grounded_pct: self.grounded_pct,
            };
        }

        // Full pool always steps physics — this is what keeps balloons
        // "already in flight" when the slider reveals more of them.
        for b in &mut self.balloons {
            b.step(dt_seconds, &self.wind, &mut self.rng);
        }

        let visible = &self.balloons[..self.visible_count];

        self.tick_count += 1;
        let recompute_links = self.tick_count % LINK_UPDATE_EVERY_N_TICKS as u64 == 0;

        let edges = if recompute_links {
            let max_range_km = 2.0 * horizon_km(BALLOON_MAX_ALT, self.horizon_refraction_coeff);
            let grid_edges = compute_grid_edges(
                visible,
                &self.towers,
                &mut self.grid,
                max_range_km,
                self.horizon_refraction_coeff,
            );

            self.union_find.clear();
            for t in &self.towers {
                self.union_find.make_set(&format!("t{}", t.id));
            }
            for b in visible {
                self.union_find.make_set(&format!("b{}", b.id));
            }
            for e in &grid_edges {
                self.union_find.union(&e.a_key, &e.b_key);
            }
            let mut grounded_roots = std::collections::HashSet::new();
            for t in &self.towers {
                grounded_roots.insert(self.union_find.find(&format!("t{}", t.id)));
            }

            // Mesh-health readout. Mean degree is the quantity that actually
            // governs connectivity: the balloon-count and horizon sliders are
            // two ways of moving the same number, and the mesh percolates
            // around degree ~4.5 (measured in bin/mesh_depth.rs). Surfacing it
            // keeps a slider drag from walking blindly across that transition.
            if !visible.is_empty() {
                let mut degree: std::collections::HashMap<String, u32> =
                    std::collections::HashMap::new();
                for e in &grid_edges {
                    *degree.entry(e.a_key.clone()).or_insert(0) += 1;
                    *degree.entry(e.b_key.clone()).or_insert(0) += 1;
                }
                let mut deg_total: u64 = 0;
                let mut grounded_count: u64 = 0;
                for b in visible {
                    let key = format!("b{}", b.id);
                    deg_total += degree.get(&key).copied().unwrap_or(0) as u64;
                    if grounded_roots.contains(&self.union_find.find(&key)) {
                        grounded_count += 1;
                    }
                }
                let n = visible.len() as f64;
                self.mean_degree = deg_total as f64 / n;
                self.grounded_pct = 100.0 * grounded_count as f64 / n;
            } else {
                self.mean_degree = 0.0;
                self.grounded_pct = 0.0;
            }

            let by_key = |key: &str| -> (f64, f64, f64) {
                if let Some(id_str) = key.strip_prefix('b') {
                    let id: u32 = id_str.parse().unwrap();
                    let b = self.balloons.iter().find(|b| b.id == id).unwrap();
                    (b.lon, b.lat, b.alt)
                } else {
                    let id: u32 = key[1..].parse().unwrap();
                    let t = self.towers.iter().find(|t| t.id == id).unwrap();
                    (t.lon, t.lat, t.height_m)
                }
            };

            Some(
                grid_edges
                    .into_iter()
                    .map(|e| {
                        let root = self.union_find.find(&e.a_key);
                        let grounded = grounded_roots.contains(&root);
                        let pair_key = if e.a_key < e.b_key {
                            format!("{}|{}", e.a_key, e.b_key)
                        } else {
                            format!("{}|{}", e.b_key, e.a_key)
                        };
                        EdgeSnapshot { pair_key, a: by_key(&e.a_key), b: by_key(&e.b_key), grounded }
                    })
                    .collect(),
            )
        } else {
            None
        };

        Snapshot {
            tick: self.tick_count,
            balloons: visible.to_vec(),
            towers: self.towers.clone(),
            edges,
            horizon_refraction_coeff: self.horizon_refraction_coeff,
            paused: false,
            mean_degree: self.mean_degree,
            grounded_pct: self.grounded_pct,
        }
    }
}
