// The single authoritative simulation loop. One task owns `World` exclusively
// (see main.rs) — no locks, mutations arrive as `Command`s over a channel and
// snapshots go out as JSON over a broadcast channel. Mirrors main.js's
// tick() (balloon motion every tick, link detection throttled to every
// LINK_UPDATE_EVERY_N_TICKS ticks).

use crate::balloon::Balloon;
use crate::config::*;
use crate::geo::{horizon_km, random_global_position};
use crate::link_detection::{compute_grid_edges, wire_pair_key, NodeKey};
use crate::mesh_adjacency::MeshAdjacency;
use crate::protocol::{MeshProtocol, ProtocolSpec, StepCtx};
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
    /// The first *query* (not mutation) command — every other variant is
    /// fire-and-forget. GET /api/balloons/:id/comms (docs/design/MESH_COMMS_DESIGN.md
    /// §3/C4) needs a read of live `World` state, and `World` is only ever
    /// touched from the single task that owns it (see main.rs), so a request
    /// has to round-trip through the same command channel and get its answer
    /// back over a oneshot.
    QueryBalloonComms { id: u32, respond_to: tokio::sync::oneshot::Sender<Option<BalloonComms>> },
}

/// What the C4 animated-packet view reads for one balloon: its belief state
/// (already public) plus its own most recently *originated* bundle's fate —
/// server truth, since e.g. satellite delivery is silent to the balloon
/// itself (see bundle.rs). `None` fields mean "not resolved yet", not "no
/// data" — a `Pending` bundle has no path/channel/ack info yet by construction.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BalloonComms {
    pub id: u32,
    pub believed_hops: Option<u32>,
    pub grounded: bool,
    pub last_bundle: Option<LastBundleView>,
    /// The full retained log (`Balloon::log`, bounded by `COMMS_LOG_CAPACITY`),
    /// newest first — the C4 comms-log panel (§3). Each record now carries its
    /// own resolution outcome (see `bundle::snapshot_resolved`), so this is a
    /// history, not just the latest bundle's fate.
    pub log: Vec<crate::telemetry::TelemetryRecord>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LastBundleView {
    pub seq: u64,
    pub state: crate::protocol::dv_dtn::bundle::AckState,
    pub channel: Option<crate::protocol::dv_dtn::bundle::Channel>,
    /// The tower that took delivery. `None` for satellite delivery, dead
    /// ends, and while the bundle is still Pending.
    pub tower_id: Option<u32>,
    /// Full recorded path (origin to tower-adjacent balloon), inclusive of
    /// both ends. `None` while the bundle is still Pending.
    pub path: Option<Vec<u32>>,
    /// How many hops of the reverse ack path completed before it was lost.
    /// Only meaningful for a `TimedOut` bundle that *did* reach a tower.
    pub ack_hops_completed: Option<u32>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
/// One radio link: which two nodes, and whether their cluster reaches a tower.
///
/// Deliberately carries no positions. `pair_key` names both endpoints ("b12|t3")
/// and the client already has every balloon and tower position in the same
/// snapshot, so shipping coordinates here restated each node's position once
/// per edge it appears in — at a mean degree of ~7 that was about 8 copies of
/// every position, and 83% of the whole snapshot. The client looked them up by
/// id and overwrote them on the same tick regardless.
pub struct EdgeSnapshot {
    pub pair_key: String,
    pub grounded: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
/// One beacon transmission, for the frontend's beacon-wavefront animation
/// (docs/design/MESH_COMMS_DESIGN.md §3). `from`/`to` are wire node keys —
/// same format and same `NodeKey` `Display` impl edges already use — so the
/// frontend's existing `parseNodeKey`/position-lookup needs no changes.
pub struct BeaconHopWire {
    pub from: String,
    pub to: String,
    pub tower_id: u32,
    pub hop_count: u32,
    pub epoch: u64,
}

/// Milliseconds since the Unix epoch. Saturates rather than panicking on a
/// clock before 1970, which is not a real case but is not worth a panic path.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub tick: u64,
    /// Wall clock at the moment this snapshot was *built*, so a client can say
    /// how stale its view is with `Date.now() - serverTimeMs` rather than
    /// diffing tick counters against a second connection and converting by an
    /// assumed tick rate — which is exactly the assumption that breaks when
    /// something is wrong.
    ///
    /// Deliberately stamped at construction and not at send time: a slow
    /// client's delay accrues *after* this point (see `handle_socket`), so a
    /// send-time stamp would read near zero and hide the thing worth measuring.
    ///
    /// Not simulation state — nothing in `World` reads it, and no experiment
    /// binary serializes a `Snapshot`, so it cannot make a seeded sweep
    /// irreproducible.
    pub server_time_ms: u64,
    pub balloons: Vec<Balloon>,
    pub towers: Vec<Tower>,
    /// `None` on ticks where links weren't recomputed (still throttled the
    /// same way main.js throttles it) — client keeps the last edge set.
    pub edges: Option<Vec<EdgeSnapshot>>,
    /// Beacon transmissions from this comms round, across all towers.
    /// `None`/omitted when none fired. Broadcast unfiltered to every client,
    /// same as `edges` — picking which tower to animate is a client-side
    /// concern (see BeaconLayer in the frontend), not server-side selection
    /// state, so multiple tabs can watch different towers independently.
    pub beacon_hops: Option<Vec<BeaconHopWire>>,
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
    protocol: Box<dyn MeshProtocol>,
    adjacency: MeshAdjacency,
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
            protocol: ProtocolSpec::default().build(),
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
        self.protocol.clear_nodes();
        for _ in 0..n {
            let (lon, lat) = random_global_position(&mut self.rng);
            let alt = BALLOON_MIN_ALT + self.rng.gen_range(0.0..(BALLOON_MAX_ALT - BALLOON_MIN_ALT));
            let b = Balloon::new(self.next_balloon_id, lon, lat, alt);
            // Stagger duty-cycle phases so the fleet doesn't transmit in
            // unison. Drawn here, inside the same iteration that builds the
            // balloon, so the RNG stream stays exactly as it was — see
            // DvDtn::spawn_node.
            self.protocol.spawn_node(&mut self.rng);
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
        self.protocol.add_tower(self.next_tower_id);
        self.next_tower_id += 1;
    }

    fn count_carrying(&self) -> u64 {
        self.protocol.carrying(self.visible_count)
    }

    /// Holding a bundle but currently believing no route — waiting, not lost.
    fn count_stranded(&self) -> u64 {
        self.protocol.stranded(self.visible_count)
    }

    /// Cumulative bundle outcomes, for offline harnesses. Panics if the world
    /// isn't running dv-dtn — these counters are that protocol's own, and a
    /// harness asking for them has already assumed which protocol it drives.
    pub fn bundle_stats(&self) -> crate::protocol::dv_dtn::bundle::BundleStats {
        self.dv_dtn().stats
    }

    /// Read-only access to dv-dtn's internals, for offline harnesses that need
    /// to inspect queues/logs directly (see bin/telemetry_records.rs). Node
    /// `i` here is the same node as `balloons[i]`.
    pub fn dv_dtn(&self) -> &crate::protocol::dv_dtn::DvDtn {
        self.protocol
            .as_any()
            .downcast_ref::<crate::protocol::dv_dtn::DvDtn>()
            .expect("world is not running the dv-dtn protocol")
    }

    /// Stop all origination — for offline drain-phase checks (see
    /// bin/bundle_delivery.rs), not something the live server ever does.
    pub fn halt_origination(&mut self) {
        self.protocol.halt_origination();
    }

    pub fn remove_tower(&mut self, id: u32) {
        self.towers.retain(|t| t.id != id);
        self.protocol.remove_tower(id);
    }

    pub fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::SetBalloonCount(n) => self.set_visible_count(n),
            Command::AddTower { lon, lat, height_m } => self.add_tower(lon, lat, height_m),
            Command::RemoveTower { id } => self.remove_tower(id),
            Command::SetHorizonRefractionCoeff(c) => self.horizon_refraction_coeff = c,
            Command::SetPaused(p) => self.paused = p,
            Command::QueryBalloonComms { id, respond_to } => {
                // Reads `last_resolved`, not the live `outstanding` — the
                // latter resets to `Pending` the instant a new bundle
                // originates, which would make the query flash back to
                // "nothing to show" between originations. See
                // `Balloon::last_resolved`.
                let comms = self.balloons.get(id as usize).map(|b| BalloonComms {
                    id: b.id,
                    believed_hops: b.believed_hops,
                    grounded: b.grounded,
                    last_bundle: self.protocol.last_bundle(id as usize),
                    log: self.protocol.log(id as usize),
                });
                // Best-effort: a dropped receiver just means the HTTP request
                // that asked was already cancelled (client disconnected).
                let _ = respond_to.send(comms);
            }
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
                server_time_ms: now_ms(),
                balloons: visible.to_vec(),
                towers: self.towers.clone(),
                edges: None,
                beacon_hops: None,
                horizon_refraction_coeff: self.horizon_refraction_coeff,
                paused: true,
                mean_degree: self.mean_degree,
                grounded_pct: self.grounded_pct,
                believed_grounded_pct: self.believed_grounded_pct,
                belief_stale_pct: self.belief_stale_pct,
                belief_unaware_pct: self.belief_unaware_pct,
                bundles_delivered: self.protocol.delivered(),
                bundles_lost: self.protocol.resolved() - self.protocol.delivered(),
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
                self.union_find.make_set(NodeKey::Tower(t.id));
            }
            for b in &self.balloons[..self.visible_count] {
                self.union_find.make_set(NodeKey::Balloon(b.id));
            }
            for e in &grid_edges {
                self.union_find.union(e.a, e.b);
            }
            let mut grounded_roots = std::collections::HashSet::new();
            for t in &self.towers {
                grounded_roots.insert(self.union_find.find(NodeKey::Tower(t.id)));
            }

            // Mesh-health readout. Mean degree is the quantity that actually
            // governs connectivity: the balloon-count and horizon sliders are
            // two ways of moving the same number, and the mesh percolates
            // around degree ~4.5 (measured in bin/mesh_depth.rs). Surfacing it
            // keeps a slider drag from walking blindly across that transition.
            let mut degree: std::collections::HashMap<NodeKey, u32> =
                std::collections::HashMap::new();
            for e in &grid_edges {
                *degree.entry(e.a).or_insert(0) += 1;
                *degree.entry(e.b).or_insert(0) += 1;
            }
            let mut deg_total: u64 = 0;
            let mut grounded_count: u64 = 0;
            for i in 0..self.visible_count {
                let key = NodeKey::Balloon(self.balloons[i].id);
                deg_total += degree.get(&key).copied().unwrap_or(0) as u64;
                // Ground truth, stamped onto the balloon for the UI only. The
                // beacon protocol must never consult this — see beacon.rs.
                let grounded = grounded_roots.contains(&self.union_find.find(key));
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

            Some(
                grid_edges
                    .into_iter()
                    .map(|e| {
                        let root = self.union_find.find(e.a);
                        let grounded = grounded_roots.contains(&root);
                        let pair_key = wire_pair_key(e.a, e.b);
                        EdgeSnapshot { pair_key, grounded }
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
        let mut beacon_hops: Option<Vec<BeaconHopWire>> = None;
        if self.tick_count % COMMS_EVERY_N_TICKS == 0 {
            let round = self.tick_count / COMMS_EVERY_N_TICKS;
            // Beacons first, so a bundle forwarded this round uses the freshest
            // belief available rather than one a round old. `awake` is the set
            // of radios that transmitted; bundles ride the same duty cycle.
            let events = self.protocol.step(
                StepCtx {
                    round,
                    balloons: &self.balloons[..self.visible_count],
                    towers: &self.towers,
                    adj: &self.adjacency,
                },
                &mut self.rng,
            );
            if !events.is_empty() {
                beacon_hops = Some(
                    events
                        .iter()
                        .map(|e| BeaconHopWire {
                            from: e.from.to_string(),
                            to: e.to.to_string(),
                            tower_id: e.tower_id.unwrap_or(0),
                            hop_count: e.hop_count.unwrap_or(0),
                            epoch: e.epoch.unwrap_or(0),
                        })
                        .collect(),
                );
            }
        }

        // Publish each balloon's *belief* and tally how far it has drifted
        // from truth. `stale` = believes it has a route but doesn't; `unaware`
        // = has a route but doesn't know it. Both are expected, not errors.
        let mut believes = 0u64;
        let mut stale = 0u64;
        let mut unaware = 0u64;
        for (i, b) in self.balloons[..self.visible_count].iter_mut().enumerate() {
            // Copy the protocol's published view onto the balloon, which is
            // what actually gets serialized to clients.
            let view = self.protocol.node_view(i);
            b.believed_hops = view.route_hops;
            b.last_channel = view.last_channel;
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
            server_time_ms: now_ms(),
            balloons: self.balloons[..self.visible_count].to_vec(),
            towers: self.towers.clone(),
            edges,
            beacon_hops,
            horizon_refraction_coeff: self.horizon_refraction_coeff,
            paused: false,
            mean_degree: self.mean_degree,
            grounded_pct: self.grounded_pct,
            believed_grounded_pct: self.believed_grounded_pct,
            belief_stale_pct: self.belief_stale_pct,
            belief_unaware_pct: self.belief_unaware_pct,
            bundles_delivered: self.protocol.delivered(),
            bundles_lost: self.protocol.resolved() - self.protocol.delivered(),
            bundles_in_flight: self.count_carrying(),
            bundles_stranded: self.count_stranded(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_world() -> World {
        let mut world = World::new(Arc::new(WindField::zero()));
        world.spawn_balloon_pool(10);
        world.set_visible_count(10);
        world
    }

    #[test]
    fn snapshots_are_stamped_with_the_current_wall_clock() {
        let before = now_ms();
        let snapshot = test_world().tick(1.0);
        let after = now_ms();
        assert!(
            snapshot.server_time_ms >= before && snapshot.server_time_ms <= after,
            "stamp {} outside [{}, {}]",
            snapshot.server_time_ms,
            before,
            after
        );
    }

    #[test]
    fn the_paused_path_is_stamped_too() {
        // `tick` builds a Snapshot in two places — the paused early return and
        // the normal path. A field added to one and missed in the other still
        // compiles, so pin the branch that is easy to forget.
        let mut world = test_world();
        world.paused = true;
        let before = now_ms();
        let snapshot = world.tick(1.0);
        assert!(snapshot.paused);
        assert!(snapshot.server_time_ms >= before);
    }

    #[test]
    fn the_stamp_advances_across_ticks() {
        let mut world = test_world();
        let first = world.tick(1.0).server_time_ms;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = world.tick(1.0).server_time_ms;
        assert!(second > first, "{second} should be later than {first}");
    }
}
