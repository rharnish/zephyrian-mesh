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
    /// Share that *believe* they have a route (from beacons they received).
    pub believed_grounded_pct: f64,
    /// Share believing in a route they no longer have — belief outliving truth.
    pub belief_stale_pct: f64,
    /// Share with a real route they haven't been told about yet.
    pub belief_unaware_pct: f64,
    /// Telemetry bundles delivered to a tower over the radio mesh, cumulative.
    pub bundles_delivered: u64,
    /// Bundles that left circulation without arriving — looped, ran out of hop
    /// budget, hit a busy relay, or aged out. Cumulative.
    pub bundles_lost: u64,
    /// Bundles currently being carried by some balloon.
    pub bundles_in_flight: u64,
    /// Bundles being held by a balloon that currently believes no route —
    /// waiting rather than lost. This is the delay-tolerant part, made visible.
    pub bundles_stranded: u64,
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
    believed_grounded_pct: f64,
    belief_stale_pct: f64,
    belief_unaware_pct: f64,
    bundle_stats: crate::bundle::BundleStats,
    adjacency: crate::beacon::MeshAdjacency,
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
            believed_grounded_pct: 0.0,
            belief_stale_pct: 0.0,
            belief_unaware_pct: 0.0,
            bundle_stats: Default::default(),
            adjacency: Default::default(),
            next_balloon_id: 0,
            next_tower_id: 0,
            grid: SpatialGrid::new(GRID_CELL_SIZE_DEG),
            union_find: UnionFind::new(),
            rng: StdRng::from_entropy(),
            tick_count: 0,
        }
    }

    /// Reseeds the RNG driving balloon spawn/drift/duty-cycle jitter. `new`
    /// defaults to entropy (right for a live server); offline harnesses that
    /// want a reproducible run per parameter combo should call this before
    /// `spawn_balloon_pool` so spawn positions/altitudes/jitter are pinned too.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = StdRng::seed_from_u64(seed);
        self
    }

    /// Spawns the full always-on pool. Call once at startup.
    pub fn spawn_balloon_pool(&mut self, n: u32) {
        self.balloons.clear();
        for _ in 0..n {
            let (lon, lat) = random_global_position(&mut self.rng);
            let alt = BALLOON_MIN_ALT + self.rng.gen_range(0.0..(BALLOON_MAX_ALT - BALLOON_MIN_ALT));
            let mut b = Balloon::new(self.next_balloon_id, lon, lat, alt);
            // Stagger duty-cycle phases so the fleet doesn't transmit in unison.
            b.next_beacon_round = crate::beacon::initial_slot(&mut self.rng);
            b.next_bundle_round = self.rng.gen_range(0..BUNDLE_INTERVAL_ROUNDS);
            self.balloons.push(b);
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

    fn count_carrying(&self) -> u64 {
        self.balloons[..self.visible_count].iter().map(|b| b.queue.len() as u64).sum()
    }

    /// Holding a bundle but currently believing no route — waiting, not lost.
    fn count_stranded(&self) -> u64 {
        self.balloons[..self.visible_count]
            .iter()
            .filter(|b| !b.queue.is_empty() && b.belief.is_none())
            .map(|b| b.queue.len() as u64)
            .sum()
    }

    /// Cumulative bundle outcomes, for offline harnesses.
    pub fn bundle_stats(&self) -> crate::bundle::BundleStats {
        self.bundle_stats
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
                believed_grounded_pct: self.believed_grounded_pct,
                belief_stale_pct: self.belief_stale_pct,
                belief_unaware_pct: self.belief_unaware_pct,
                bundles_delivered: self.bundle_stats.delivered,
                bundles_lost: self.bundle_stats.resolved() - self.bundle_stats.delivered,
                bundles_in_flight: self.count_carrying(),
                bundles_stranded: self.count_stranded(),
            };
        }

        // Full pool always steps physics — this is what keeps balloons
        // "already in flight" when the slider reveals more of them.
        for b in &mut self.balloons {
            b.step(dt_seconds, &self.wind, &mut self.rng);
        }

        self.tick_count += 1;
        let recompute_links = self.tick_count % LINK_UPDATE_EVERY_N_TICKS as u64 == 0;

        let edges = if recompute_links {
            let max_range_km = 2.0 * horizon_km(BALLOON_MAX_ALT, self.horizon_refraction_coeff);
            let grid_edges = compute_grid_edges(
                &self.balloons[..self.visible_count],
                &self.towers,
                &mut self.grid,
                max_range_km,
                self.horizon_refraction_coeff,
            );

            self.union_find.clear();
            for t in &self.towers {
                self.union_find.make_set(&format!("t{}", t.id));
            }
            for b in &self.balloons[..self.visible_count] {
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
            let mut degree: std::collections::HashMap<String, u32> =
                std::collections::HashMap::new();
            for e in &grid_edges {
                *degree.entry(e.a_key.clone()).or_insert(0) += 1;
                *degree.entry(e.b_key.clone()).or_insert(0) += 1;
            }
            let mut deg_total: u64 = 0;
            let mut grounded_count: u64 = 0;
            for i in 0..self.visible_count {
                let key = format!("b{}", self.balloons[i].id);
                deg_total += degree.get(&key).copied().unwrap_or(0) as u64;
                // Ground truth, stamped onto the balloon for the UI only. The
                // beacon protocol must never consult this — see beacon.rs.
                let grounded = grounded_roots.contains(&self.union_find.find(&key));
                self.balloons[i].grounded = grounded;
                if grounded {
                    grounded_count += 1;
                }
            }
            if self.visible_count > 0 {
                let n = self.visible_count as f64;
                self.mean_degree = deg_total as f64 / n;
                self.grounded_pct = 100.0 * grounded_count as f64 / n;
            } else {
                self.mean_degree = 0.0;
                self.grounded_pct = 0.0;
            }

            // Who can hear whom, for the beacon flood below.
            self.adjacency.rebuild(&grid_edges, self.visible_count, &self.towers);

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

        // Decentralized discovery, on the comms clock rather than the tick
        // clock (see config::COMMS_EVERY_N_TICKS). Beacon slots are per-node
        // and jittered, so they don't align with the link-recompute cadence.
        // Only visible balloons take part, since only they have edges.
        if self.tick_count % COMMS_EVERY_N_TICKS == 0 {
            let round = self.tick_count / COMMS_EVERY_N_TICKS;
            // Beacons first, so a bundle forwarded this round uses the freshest
            // belief available rather than one a round old. `awake` is the set
            // of radios that transmitted; bundles ride the same duty cycle.
            let awake = crate::beacon::step(
                &mut self.balloons[..self.visible_count],
                &mut self.towers,
                &self.adjacency,
                round,
                &mut self.rng,
            );
            crate::bundle::step(
                &mut self.balloons[..self.visible_count],
                &self.adjacency,
                &awake,
                round,
                &mut self.bundle_stats,
            );
        }

        // Publish each balloon's *belief* and tally how far it has drifted
        // from truth. `stale` = believes it has a route but doesn't; `unaware`
        // = has a route but doesn't know it. Both are expected, not errors.
        let mut believes = 0u64;
        let mut stale = 0u64;
        let mut unaware = 0u64;
        for b in &mut self.balloons[..self.visible_count] {
            b.believed_hops = b.belief.map(|x| x.hop_count);
            match (b.believed_hops.is_some(), b.grounded) {
                (true, true) => believes += 1,
                (true, false) => {
                    believes += 1;
                    stale += 1;
                }
                (false, true) => unaware += 1,
                (false, false) => {}
            }
        }
        let n = self.visible_count.max(1) as f64;
        self.believed_grounded_pct = 100.0 * believes as f64 / n;
        self.belief_stale_pct = 100.0 * stale as f64 / n;
        self.belief_unaware_pct = 100.0 * unaware as f64 / n;

        Snapshot {
            tick: self.tick_count,
            balloons: self.balloons[..self.visible_count].to_vec(),
            towers: self.towers.clone(),
            edges,
            horizon_refraction_coeff: self.horizon_refraction_coeff,
            paused: false,
            mean_degree: self.mean_degree,
            grounded_pct: self.grounded_pct,
            believed_grounded_pct: self.believed_grounded_pct,
            belief_stale_pct: self.belief_stale_pct,
            belief_unaware_pct: self.belief_unaware_pct,
            bundles_delivered: self.bundle_stats.delivered,
            bundles_lost: self.bundle_stats.resolved() - self.bundle_stats.delivered,
            bundles_in_flight: self.count_carrying(),
            bundles_stranded: self.count_stranded(),
        }
    }
}
