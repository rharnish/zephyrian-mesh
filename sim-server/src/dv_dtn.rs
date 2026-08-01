// Owner of all state belonging to the shipped comms protocol: periodic
// distance-vector discovery (beacon.rs) feeding store-carry-forward DTN
// delivery (bundle.rs).
//
// Why this exists: that state used to live as ten fields on `Balloon` and two
// on `Tower`, which made "the protocol" a thing smeared across the physics
// structs rather than a component with a boundary. A protocol that routes
// differently needs *different* state — epidemic keeps a copy count per
// bundle and no route at all, gossip keeps a partial topology map — and none
// of those fit a `belief: Option<RouteBelief>` field on a balloon.
//
// So the balloon keeps physics and a small published view; everything the
// protocol needs to do its job lives here, indexed in parallel with the
// visible balloon slice. `DvNode` is this protocol's per-node state, and
// swapping protocols swaps the whole struct rather than reinterpreting
// shared fields.

use crate::balloon::Balloon;
use crate::beacon::{self, BeaconHop, MeshAdjacency, RouteBelief};
use crate::bundle::{self, Ack, Bundle, Channel, OutstandingBundle, ResolvedBundle};
use crate::bundle_stats::BundleStats;
use crate::config::BUNDLE_INTERVAL_ROUNDS;
use crate::telemetry::TelemetryRecord;
use crate::tower::Tower;
use rand::Rng;
use std::collections::{HashMap, VecDeque};

/// One balloon's protocol state. Indexed in parallel with the visible balloon
/// slice — index `i` here is the same node as `balloons[i]`, the same
/// correspondence `MeshAdjacency` already relies on.
#[derive(Debug, Clone, Default)]
pub struct DvNode {
    /// What this balloon believes about reaching a tower, learned only from
    /// beacons that physically arrived (see beacon.rs).
    pub belief: Option<RouteBelief>,
    /// Next comms round this balloon is awake to transmit (radio duty cycle).
    pub next_beacon_round: u64,
    /// Bundles being carried, oldest first. Bounded by RELAY_QUEUE_CAPACITY.
    pub queue: VecDeque<Bundle>,
    /// Acks in transit being carried. Bounded by ACK_QUEUE_CAPACITY.
    pub ack_queue: VecDeque<Ack>,
    /// Next comms round this balloon may originate a bundle.
    pub next_bundle_round: u64,
    /// Monotonic per-balloon sequence for bundles it originates; also stamps
    /// the telemetry record created alongside each one, since they are the
    /// same event.
    pub bundle_seq: u64,
    /// Telemetry measured and retained — the copy that *stays*, as against the
    /// copy inside each bundle that travels. Bounded by COMMS_LOG_CAPACITY.
    pub log: VecDeque<TelemetryRecord>,
    /// This balloon's own view of its most recently originated bundle —
    /// deliberately poorer than server truth.
    pub outstanding: Option<OutstandingBundle>,
    /// A settled copy of `outstanding`, frozen when it stops being `Pending`
    /// so the next origination can't clobber it.
    pub last_resolved: Option<ResolvedBundle>,
    /// Server truth about how the last resolved bundle actually got through.
    /// Published onto `Balloon::last_channel` each tick for the UI.
    pub last_channel: Option<Channel>,
}

/// A tower's beacon scheduling state. Lives here rather than on `Tower` for
/// the same reason as the balloon fields: a protocol that doesn't flood from
/// towers has no use for it.
#[derive(Debug, Clone, Default)]
pub struct TowerBeacon {
    /// Wave counter, incremented per transmission so a balloon can tell a
    /// fresh wave from a stale one.
    pub epoch: u64,
    /// Next comms round this tower emits.
    pub next_round: u64,
}

pub struct DvDtn {
    pub nodes: Vec<DvNode>,
    /// Keyed by tower id, not by slot: tower ids are never reused, but slots
    /// shift whenever a tower is removed.
    pub towers: HashMap<u32, TowerBeacon>,
    pub stats: BundleStats,
}

impl Default for DvDtn {
    fn default() -> Self {
        DvDtn { nodes: Vec::new(), towers: HashMap::new(), stats: BundleStats::default() }
    }
}

impl DvDtn {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear_nodes(&mut self) {
        self.nodes.clear();
    }

    /// Adds one node's state, drawing its duty-cycle phases. Called from
    /// `World::spawn_balloon_pool` inside the same loop iteration that builds
    /// the balloon, so the RNG draw order — position, altitude, beacon phase,
    /// bundle phase — stays exactly as it was when this lived on `Balloon`.
    /// Changing that order reseeds every balloon in every seeded sweep.
    pub fn spawn_node(&mut self, rng: &mut impl Rng) {
        self.nodes.push(DvNode {
            next_beacon_round: beacon::initial_slot(rng),
            next_bundle_round: rng.gen_range(0..BUNDLE_INTERVAL_ROUNDS),
            ..Default::default()
        });
    }

    pub fn add_tower(&mut self, id: u32) {
        self.towers.insert(id, TowerBeacon::default());
    }

    pub fn remove_tower(&mut self, id: u32) {
        self.towers.remove(&id);
    }

    /// Advance the protocol by one comms round over the visible slice.
    /// `balloons` is read-only — the protocol reads identity and position, and
    /// may not write physics. Returns this round's beacon transmissions, for
    /// the frontend's wavefront animation.
    ///
    /// Beacons run before bundles so a bundle forwarded this round acts on the
    /// freshest belief rather than one a round old, and bundles reuse the
    /// beacon's `awake` set rather than keeping a schedule of their own: a
    /// radio that is awake is awake for both.
    pub fn step(
        &mut self,
        balloons: &[Balloon],
        towers: &[Tower],
        adj: &MeshAdjacency,
        round: u64,
        rng: &mut impl Rng,
    ) -> Vec<BeaconHop> {
        let n = balloons.len();
        let result =
            beacon::step(&mut self.nodes[..n], balloons, &mut self.towers, towers, adj, round, rng);
        bundle::step(&mut self.nodes[..n], balloons, adj, &result.awake, round, &mut self.stats);
        result.hops
    }

    /// Stop every balloon originating new bundles, so what is already in the
    /// mesh can be watched to completion. Used by the drain-phase conservation
    /// checks in bin/bundle_delivery.rs — the ones that caught both C1's and
    /// C2's "never actually resolves" bugs.
    pub fn halt_origination(&mut self) {
        for n in self.nodes.iter_mut() {
            n.next_bundle_round = u64::MAX;
        }
    }

    /// Acks currently in transit anywhere in the mesh.
    pub fn acks_in_flight(&self) -> u64 {
        self.nodes.iter().map(|n| n.ack_queue.len() as u64).sum()
    }

    /// Hops to a tower as node `i` believes, for publication onto the wire.
    pub fn believed_hops(&self, i: usize) -> Option<u32> {
        self.nodes.get(i).and_then(|n| n.belief).map(|b| b.hop_count)
    }

    /// Bundles currently held across the visible slice.
    pub fn carrying(&self, visible: usize) -> u64 {
        self.nodes[..visible.min(self.nodes.len())].iter().map(|n| n.queue.len() as u64).sum()
    }

    /// Bundles held by a balloon that believes it has no route — waiting
    /// rather than lost. This is the delay-tolerant part, made countable.
    pub fn stranded(&self, visible: usize) -> u64 {
        self.nodes[..visible.min(self.nodes.len())]
            .iter()
            .filter(|n| !n.queue.is_empty() && n.belief.is_none())
            .map(|n| n.queue.len() as u64)
            .sum()
    }
}
