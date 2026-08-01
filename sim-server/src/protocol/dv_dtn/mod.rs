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

pub mod beacon;
pub mod bundle;
pub mod bundle_stats;
pub mod params;

use crate::protocol::{
    Capabilities, CommsEvent, EventKind, LastBundleView, MeshProtocol, NodeCommsView, StepCtx,
};
use crate::telemetry::TelemetryRecord;
use beacon::RouteBelief;
use bundle::{Ack, Bundle, Channel, OutstandingBundle, ResolvedBundle};
use bundle_stats::BundleStats;
use params::DvDtnParams;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
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

/// Everything this protocol can express — all of it, as it happens, since the
/// UI was built against this protocol in the first place.
static CAPABILITIES: Capabilities = Capabilities {
    name: "dv-dtn",
    label: "Distance-vector + store-and-forward",
    route_belief: true,
    next_hop_paths: true,
    acks: true,
    satellite_fallback: true,
    event_kinds: &[EventKind::RouteAd, EventKind::Bundle, EventKind::Ack],
};

pub struct DvDtn {
    pub params: DvDtnParams,
    /// The protocol's own randomness — duty-cycle phases and slot jitter.
    /// Separate from the world's stream on purpose; see `MeshProtocol::reseed`.
    rng: StdRng,
    pub nodes: Vec<DvNode>,
    /// Keyed by tower id, not by slot: tower ids are never reused, but slots
    /// shift whenever a tower is removed.
    pub towers: HashMap<u32, TowerBeacon>,
    pub stats: BundleStats,
}

impl Default for DvDtn {
    fn default() -> Self {
        DvDtn {
            params: DvDtnParams::default(),
            rng: StdRng::from_entropy(),
            nodes: Vec::new(),
            towers: HashMap::new(),
            stats: BundleStats::default(),
        }
    }
}

impl DvDtn {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_params(params: DvDtnParams) -> Self {
        DvDtn { params, ..Self::default() }
    }

    /// Acks currently in transit anywhere in the mesh.
    pub fn acks_in_flight(&self) -> u64 {
        self.nodes.iter().map(|n| n.ack_queue.len() as u64).sum()
    }

}

impl MeshProtocol for DvDtn {
    fn spec_name(&self) -> &'static str {
        "dv-dtn"
    }

    fn capabilities(&self) -> &'static Capabilities {
        &CAPABILITIES
    }

    fn reseed(&mut self, seed: u64) {
        self.rng = StdRng::seed_from_u64(seed);
    }

    fn clear_nodes(&mut self) {
        self.nodes.clear();
    }

    /// The two draws here are the balloon's beacon and bundle duty-cycle
    /// phases, staggered so the fleet doesn't transmit in unison.
    fn spawn_node(&mut self) {
        let next_beacon_round = beacon::initial_slot(&self.params, &mut self.rng);
        let next_bundle_round = self.rng.gen_range(0..self.params.bundle_interval_rounds);
        self.nodes.push(DvNode { next_beacon_round, next_bundle_round, ..Default::default() });
    }

    fn add_tower(&mut self, id: u32) {
        self.towers.insert(id, TowerBeacon::default());
    }

    fn remove_tower(&mut self, id: u32) {
        self.towers.remove(&id);
    }

    /// Beacons run before bundles so a bundle forwarded this round acts on the
    /// freshest belief rather than one a round old, and bundles reuse the
    /// beacon's `awake` set rather than keeping a schedule of their own: a
    /// radio that is awake is awake for both.
    fn step(&mut self, ctx: StepCtx<'_>) -> Vec<CommsEvent> {
        let n = ctx.balloons.len();
        let result = beacon::step(
            &mut self.nodes[..n],
            ctx.balloons,
            &mut self.towers,
            ctx.towers,
            ctx.adj,
            &self.params,
            ctx.round,
            &mut self.rng,
        );
        let mut events: Vec<CommsEvent> = bundle::step(
            &mut self.nodes[..n],
            ctx.balloons,
            ctx.adj,
            &self.params,
            &result.awake,
            ctx.round,
            &mut self.stats,
        );
        events.extend(result.hops.into_iter().map(|h| CommsEvent {
            kind: EventKind::RouteAd,
            from: h.from,
            to: crate::link_detection::NodeKey::Balloon(ctx.balloons[h.to_balloon].id),
            payload: 1,
            tower_id: Some(h.tower_id),
            hop_count: Some(h.hop_count),
            epoch: Some(h.epoch),
        }));
        events
    }

    fn node_view(&self, i: usize) -> NodeCommsView {
        match self.nodes.get(i) {
            Some(n) => NodeCommsView {
                route_hops: n.belief.map(|b| b.hop_count),
                last_channel: n.last_channel,
            },
            None => NodeCommsView::default(),
        }
    }

    fn carrying(&self, visible: usize) -> u64 {
        self.nodes[..visible.min(self.nodes.len())].iter().map(|n| n.queue.len() as u64).sum()
    }

    /// Bundles held by a balloon that believes it has no route — waiting
    /// rather than lost. This is the delay-tolerant part, made countable.
    fn stranded(&self, visible: usize) -> u64 {
        self.nodes[..visible.min(self.nodes.len())]
            .iter()
            .filter(|n| !n.queue.is_empty() && n.belief.is_none())
            .map(|n| n.queue.len() as u64)
            .sum()
    }

    fn delivered(&self) -> u64 {
        self.stats.delivered
    }

    fn resolved(&self) -> u64 {
        self.stats.resolved()
    }

    /// Reads `last_resolved`, not the live `outstanding` — the latter resets
    /// to `Pending` the instant a new bundle originates, which would make the
    /// query flash back to "nothing to show" between originations.
    fn last_bundle(&self, i: usize) -> Option<LastBundleView> {
        let r = self.nodes.get(i)?.last_resolved.as_ref()?;
        Some(LastBundleView {
            seq: r.seq,
            state: r.state,
            channel: r.channel,
            tower_id: r.tower_id,
            path: Some(r.path.clone()),
            ack_hops_completed: r.ack_hops_completed,
        })
    }

    fn log(&self, i: usize) -> Vec<TelemetryRecord> {
        self.nodes.get(i).map(|n| n.log.iter().rev().cloned().collect()).unwrap_or_default()
    }

    /// Used by the drain-phase conservation checks in bin/bundle_delivery.rs —
    /// the ones that caught both C1's and C2's "never actually resolves" bugs.
    fn halt_origination(&mut self) {
        for n in self.nodes.iter_mut() {
            n.next_bundle_round = u64::MAX;
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
