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

pub mod aodv;
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
use params::{Discovery, DvDtnParams};
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
    /// Under `AckPolicy::Digest`, the delivery announcements that arrived with
    /// this node's current belief, held so they relay onward with it. Empty
    /// under source-routed acks.
    pub ack_digest: Vec<(u32, u64)>,
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
    /// Deliveries this tower has taken, newest last, for `AckPolicy::Digest`.
    /// Bounded by trimming on insert and by age when announced.
    pub recent: VecDeque<(u32, u64, u64)>, // (origin_id, seq, delivered_at_round)
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

/// Same expressive power as the proactive mode; different traffic on the wire.
static CAPABILITIES_REACTIVE: Capabilities = Capabilities {
    name: "dv-dtn-reactive",
    label: "On-demand discovery + store-and-forward",
    route_belief: true,
    next_hop_paths: true,
    acks: true,
    satellite_fallback: true,
    event_kinds: &[
        EventKind::RouteRequest,
        EventKind::RouteReply,
        EventKind::Bundle,
        EventKind::Ack,
    ],
};

pub struct DvDtn {
    pub params: DvDtnParams,
    /// Only populated under `Discovery::Reactive`; the proactive mode carries
    /// none of this state.
    aodv: aodv::State,
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
            aodv: aodv::State::default(),
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

    /// Records a delivery for announcement in this tower's next beacons.
    /// Bounded well above `ack_digest_entries`, since a tower can take several
    /// bundles per contact and each beacon only announces a window of them.
    fn note_delivery(&mut self, tower_id: u32, origin_id: u32, seq: u64, round: u64) {
        let cap = self.params.ack_digest_entries.saturating_mul(8).max(64);
        if let Some(t) = self.towers.get_mut(&tower_id) {
            t.recent.push_back((origin_id, seq, round));
            while t.recent.len() > cap {
                t.recent.pop_front();
            }
        }
    }

    /// Test-only shims: driving a DvDtn directly, without a World, so a test
    /// can exercise beacon-and-bundle interaction (which is where the digest
    /// lives) rather than either half alone.
    #[cfg(test)]
    pub fn reseed_for_test(&mut self, seed: u64) {
        use rand::SeedableRng;
        self.rng = StdRng::seed_from_u64(seed);
    }

    #[cfg(test)]
    pub fn add_tower_for_test(&mut self, id: u32) {
        self.towers.insert(id, TowerBeacon::default());
    }

    #[cfg(test)]
    pub fn spawn_node_for_test(&mut self) {
        let next_beacon_round = beacon::initial_slot(&self.params, &mut self.rng);
        let next_bundle_round = self.rng.gen_range(0..self.params.bundle_interval_rounds);
        self.nodes.push(DvNode { next_beacon_round, next_bundle_round, ..Default::default() });
    }

    #[cfg(test)]
    pub fn step_for_test(
        &mut self,
        balloons: &[crate::balloon::Balloon],
        towers: &[crate::tower::Tower],
        adj: &crate::mesh_adjacency::MeshAdjacency,
        round: u64,
    ) {
        <Self as MeshProtocol>::step(self, StepCtx { round, balloons, towers, adj });
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

    /// The two discovery modes differ in what they *emit*, not in what they
    /// can express — both maintain a real route with a hop count — so the
    /// flags match and only the label and event kinds change.
    fn capabilities(&self) -> &'static Capabilities {
        match self.params.discovery {
            Discovery::Proactive => &CAPABILITIES,
            Discovery::Reactive => &CAPABILITIES_REACTIVE,
        }
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
        // Discovery is the only half that differs between the proactive and
        // reactive variants; forwarding below is identical either way, which
        // is precisely why reactive lives here rather than as its own
        // protocol. Both return the wake set bundles ride on.
        let (awake, mut discovery_events, hops, digest_acks) = match self.params.discovery {
            Discovery::Proactive => {
                let r = beacon::step(
                    &mut self.nodes[..n],
                    ctx.balloons,
                    &mut self.towers,
                    ctx.towers,
                    ctx.adj,
                    &self.params,
                    ctx.round,
                    &mut self.rng,
                );
                (r.awake, Vec::new(), r.hops, r.digest_acks)
            }
            Discovery::Reactive => {
                let (awake, ev) = aodv::step(
                    &mut self.nodes[..n],
                    &mut self.aodv,
                    ctx.balloons,
                    ctx.adj,
                    &self.params,
                    ctx.round,
                    &mut self.rng,
                );
                // Reactive discovery sends no tower beacons, so there is
                // nothing for a digest to ride on; the spec parser refuses
                // that combination outright rather than letting it look like
                // a protocol that simply never acknowledges.
                (awake, ev, Vec::new(), Vec::new())
            }
        };
        let result = beacon::BeaconStepResult { awake, hops, digest_acks };
        let out = bundle::step(
            &mut self.nodes[..n],
            ctx.balloons,
            ctx.adj,
            &self.params,
            &result.awake,
            ctx.round,
            &mut self.stats,
        );
        let mut events = out.events;
        events.append(&mut discovery_events);
        // Deliveries taken this round go into their tower's announcement ring;
        // origins hear about them on a later beacon, which is what makes the
        // receipt cost nothing.
        for d in out.deliveries {
            self.note_delivery(d.tower_id, d.origin_id, d.seq, ctx.round);
        }
        // Origins that heard their own delivery announced this round.
        for a in result.digest_acks {
            if let Some(node) = self.nodes.get_mut(a.node) {
                bundle::apply_digest_ack(node, a.seq, &mut self.stats);
            }
        }
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

    /// Keys match `BundleStats`' own field names, so a harness moving from the
    /// concrete type to the table doesn't have to relearn anything.
    fn stats(&self) -> crate::protocol::stats::StatsTable {
        use crate::protocol::stats::StatsTable;
        let st = &self.stats;
        StatsTable::new()
            .count("originated", st.originated)
            .count("delivered", st.delivered)
            .count("resolved", st.resolved())
            .ratio("completion_rate", st.completion_rate())
            .count("satellite", st.satellite)
            .count("dropped_loop", st.dropped_loop)
            .count("dropped_ttl", st.dropped_ttl)
            .count("blocked", st.blocked)
            .count("acked", st.acked)
            .count("ack_lost", st.ack_lost)
            .count("slots_with_bundle", st.slots_with_bundle)
            .count("stall_no_belief", st.stall_no_belief)
            .count("stall_stale_next_hop", st.stall_stale_next_hop)
            .count("stall_tower_gone", st.stall_tower_gone)
            .ratio("mean_tower_adjacent", st.mean_tower_adjacent())
            .ratio("delivery_ceiling_per_round", st.delivery_capacity_per_round(&self.params))
            .ratio("belief_hops_mean", bundle::hist_mean(&st.belief_hops))
            .ratio("delivered_hops_mean", bundle::hist_mean(&st.delivered_hops))
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
