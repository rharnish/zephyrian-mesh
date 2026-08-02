// Binary spray-and-wait: replication instead of routing.
//
// This exists to find out whether `MeshProtocol` is actually protocol-agnostic
// or merely dv-dtn with the serial numbers filed off. It is the awkward case
// on purpose — nothing here has a route, a next hop, or a path:
//
//   * **No discovery at all.** No beacons, no route beliefs, nothing to go
//     stale. A balloon never has an opinion about whether it can reach the
//     ground, which means the belief-vs-truth overlay this project was built
//     to show has no referent under this protocol. That is what `Capabilities`
//     is for, and this is the first protocol to actually exercise it.
//   * **A bundle is many copies, not one packet.** So there is no single
//     recorded path to replay, and "delivered" needs deduplicating: several
//     copies of the same telemetry may reach towers independently.
//   * **Forwarding is a coin flip, not a decision.** A holder hands half its
//     copy budget to a neighbour that doesn't have it yet, chosen at random.
//
// Binary spray-and-wait rather than plain epidemic because the copy budget is
// conserved: a bundle exists in at most `copies` instances network-wide, ever.
// Plain epidemic needs a per-node summary vector of everything ever seen to
// avoid re-infecting itself, which is unbounded state and a worse fit for a
// duty-cycled fleet. Spray-and-wait gets bounded flooding for free, and the
// budget is the interesting dial.
//
// Spray phase (copies > 1): hand ⌊n/2⌋ to one neighbour per wake slot, keep
// the rest. Wait phase (copies == 1): hold, and deliver only on direct contact
// with a tower. Any holder in tower range delivers regardless of phase — the
// tower is the destination, so reaching it always ends the trip.

use crate::protocol::{
    Capabilities, CommsEvent, EventKind, LastBundleView, MeshProtocol, NodeCommsView, StepCtx,
};
use crate::protocol::dv_dtn::bundle::{AckState, Channel, OutstandingBundle};
use crate::link_detection::NodeKey;
use crate::telemetry::TelemetryRecord;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::VecDeque;

pub mod params;
pub mod stats;

use params::EpidemicParams;
use stats::EpidemicStats;

/// Everything this protocol can express — notably little. Each `false` here is
/// a piece of UI that will hide itself rather than display a value with no
/// meaning behind it.
static CAPABILITIES: Capabilities = Capabilities {
    name: "epidemic",
    label: "Binary spray-and-wait (replication)",
    // No routes exist, so there is no hop count to believe and no belief to be
    // stale. Not "unknown" — absent.
    route_belief: false,
    // A bundle is several copies travelling independently; there is no single
    // path to animate.
    next_hop_paths: false,
    // Towers take delivery but nothing carries a receipt back, so an origin
    // genuinely never learns its bundle landed. Every bundle it originates
    // times out from its own point of view, however well the mesh is doing —
    // which is the belief-vs-truth gap in its most extreme form.
    acks: false,
    satellite_fallback: true,
    event_kinds: &[EventKind::Bundle],
};

/// One copy of one telemetry record, in transit.
///
/// Deliberately has no `path`: under replication there is no single route a
/// record took, so recording one would be inventing it. Loop detection isn't
/// needed either — the copy budget bounds the spread by construction.
#[derive(Debug, Clone)]
pub struct Copy_ {
    pub origin_id: u32,
    pub seq: u64,
    pub created_at_round: u64,
    pub record: TelemetryRecord,
    /// Copies this holder may still hand out. 1 means the wait phase.
    pub copies: u32,
}

impl Copy_ {
    fn age(&self, round: u64) -> u64 {
        round.saturating_sub(self.created_at_round)
    }
}

#[derive(Debug, Clone, Default)]
pub struct EpiNode {
    pub held: VecDeque<Copy_>,
    pub next_wake_round: u64,
    pub next_bundle_round: u64,
    pub bundle_seq: u64,
    pub log: VecDeque<TelemetryRecord>,
    /// The origin's own view. Under this protocol it only ever goes Pending ->
    /// TimedOut, because nothing tells it otherwise; server truth about what
    /// actually happened lives in the stats and in `last_channel`.
    pub outstanding: Option<OutstandingBundle>,
    pub last_resolved: Option<crate::protocol::dv_dtn::bundle::ResolvedBundle>,
    pub last_channel: Option<Channel>,
}

pub struct Epidemic {
    pub params: EpidemicParams,
    pub nodes: Vec<EpiNode>,
    pub stats: EpidemicStats,
    rng: StdRng,
}

impl Default for Epidemic {
    fn default() -> Self {
        Epidemic {
            params: EpidemicParams::default(),
            nodes: Vec::new(),
            stats: EpidemicStats::default(),
            rng: StdRng::from_entropy(),
        }
    }
}

impl Epidemic {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_params(params: EpidemicParams) -> Self {
        Epidemic { params, ..Self::default() }
    }

    fn next_slot(&mut self, round: u64) -> u64 {
        let j = self.params.wake_jitter_rounds;
        let jitter = self.rng.gen_range(0..=(2 * j)) as i64 - j as i64;
        round + (self.params.wake_interval_rounds as i64 + jitter).max(1) as u64
    }

    /// Marks an origin's own bundle resolved by server truth. The origin is
    /// *not* told — `outstanding` is left Pending to time out on its own,
    /// because under this protocol nothing carries that news back. The split
    /// between what is recorded here and what the balloon knows is the whole
    /// point.
    fn record_outcome(&mut self, origin_id: u32, seq: u64, channel: Channel) -> bool {
        let Some(n) = self.nodes.get_mut(origin_id as usize) else { return false };
        let already = n.last_resolved.as_ref().is_some_and(|r| r.seq == seq);
        if already {
            return false;
        }
        n.last_channel = Some(channel);
        n.last_resolved = Some(crate::protocol::dv_dtn::bundle::ResolvedBundle {
            seq,
            state: AckState::Pending,
            channel: Some(channel),
            tower_id: None,
            path: Vec::new(),
            ack_hops_completed: None,
        });
        if let Some(record) = n.log.iter_mut().find(|r| r.seq == seq) {
            record.channel = Some(channel);
        }
        true
    }
}

impl MeshProtocol for Epidemic {
    fn spec_name(&self) -> &'static str {
        "epidemic"
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

    fn spawn_node(&mut self) {
        let next_wake_round = self.rng.gen_range(0..self.params.wake_interval_rounds);
        let next_bundle_round = self.rng.gen_range(0..self.params.bundle_interval_rounds);
        self.nodes.push(EpiNode {
            next_wake_round,
            next_bundle_round,
            ..Default::default()
        });
    }

    fn add_tower(&mut self, _id: u32) {}
    fn remove_tower(&mut self, _id: u32) {}

    fn step(&mut self, ctx: StepCtx<'_>) -> Vec<CommsEvent> {
        let n = ctx.balloons.len();
        let round = ctx.round;
        let mut events = Vec::new();

        // 1. Expire. A copy past its budget is simply gone; the *origin's*
        //    bundle falls back to satellite if nothing delivered it first.
        for i in 0..n.min(self.nodes.len()) {
            let expired: Vec<(u32, u64)> = self.nodes[i]
                .held
                .iter()
                .filter(|c| c.age(round) > self.params.bundle_max_age_rounds)
                .map(|c| (c.origin_id, c.seq))
                .collect();
            self.nodes[i].held.retain(|c| c.age(round) <= self.params.bundle_max_age_rounds);
            for (origin_id, seq) in expired {
                self.stats.copies_expired += 1;
                if self.record_outcome(origin_id, seq, Channel::Satellite) {
                    self.stats.satellite += 1;
                }
            }
        }

        // 1b. The origin gives up waiting. It never learns the outcome either
        //     way — there is no receipt under this protocol — so this is
        //     always TimedOut, even for bundles that were delivered long ago.
        for i in 0..n.min(self.nodes.len()) {
            let node = &mut self.nodes[i];
            if let Some(o) = node.outstanding.as_mut() {
                if o.state == AckState::Pending
                    && round.saturating_sub(o.created_at_round)
                        > self.params.bundle_max_age_rounds
                {
                    o.state = AckState::TimedOut;
                }
            }
        }

        // 2. Wake set. Same duty-cycle model as dv-dtn: a radio transmits on
        //    its own jittered slot, which is what rations everything.
        let mut awake: Vec<usize> = Vec::new();
        for i in 0..n.min(self.nodes.len()) {
            if round < self.nodes[i].next_wake_round {
                continue;
            }
            self.nodes[i].next_wake_round = self.next_slot(round);
            awake.push(i);
        }

        // 3. Deliver, then spray. Delivery first: a holder in tower range has
        //    reached the destination, and that ends the trip whatever phase it
        //    is in.
        for &i in &awake {
            if let Some(tower_id) = ctx.adj.tower_in_range(i) {
                let take = self.params.tower_contact.min(self.nodes[i].held.len());
                if take > 0 {
                    events.push(CommsEvent {
                        kind: EventKind::Bundle,
                        from: NodeKey::Balloon(ctx.balloons[i].id),
                        to: NodeKey::Tower(tower_id),
                        payload: take as u32,
                        tower_id: Some(tower_id),
                        hop_count: None,
                        epoch: None,
                    });
                }
                for _ in 0..take {
                    let c = self.nodes[i].held.pop_front().expect("checked non-empty");
                    // Several copies of one record can arrive independently;
                    // only the first is a delivery, the rest are waste that
                    // this protocol pays for its robustness.
                    if self.record_outcome(c.origin_id, c.seq, Channel::Radio) {
                        self.stats.delivered += 1;
                        // Measured on the copy that actually arrived. Under
                        // replication that is the *fastest* of several racing
                        // copies, which is the mechanism's one advantage on
                        // this axis and the reason to measure it here at all.
                        self.stats.delivery_latency.record(round - c.created_at_round);
                    } else {
                        self.stats.duplicate_arrivals += 1;
                    }
                }
                continue; // the wake was spent on the tower contact
            }
        }

        // Spray: hand half the budget to one neighbour that doesn't hold it.
        // Two-phase like everything else here — applying in place would let a
        // copy cross several hops in one round depending on iteration order.
        let mut handoffs: Vec<(usize, u32, Copy_)> = Vec::new();
        for &i in &awake {
            if ctx.adj.tower_in_range(i).is_some() {
                continue; // already delivered this slot
            }
            let Some(pos) = self.nodes[i].held.iter().position(|c| c.copies > 1) else { continue };
            let neighbours = ctx.adj.neighbors(i);
            if neighbours.is_empty() {
                continue;
            }
            // Candidates are neighbours not already holding this record.
            let (origin_id, seq) = {
                let c = &self.nodes[i].held[pos];
                (c.origin_id, c.seq)
            };
            let candidates: Vec<u32> = neighbours
                .iter()
                .copied()
                .filter(|&nb| {
                    self.nodes
                        .get(nb as usize)
                        .is_some_and(|r| !r.held.iter().any(|c| c.origin_id == origin_id && c.seq == seq))
                })
                .collect();
            if candidates.is_empty() {
                self.stats.no_candidate += 1;
                continue;
            }
            let pick = candidates[self.rng.gen_range(0..candidates.len())];
            let give = {
                let c = &mut self.nodes[i].held[pos];
                let give = c.copies / 2;
                c.copies -= give;
                give
            };
            let mut sent = self.nodes[i].held[pos].clone();
            sent.copies = give;
            handoffs.push((i, pick, sent));
        }

        for (from, to, copy) in handoffs {
            let to_idx = to as usize;
            let has_room = self
                .nodes
                .get(to_idx)
                .is_some_and(|r| r.held.len() < self.params.relay_queue_capacity);
            if !has_room {
                // Hold: give the copies back to the sender, same
                // hold-don't-drop rule the other protocol uses.
                self.stats.blocked += 1;
                if let Some(c) = self.nodes[from]
                    .held
                    .iter_mut()
                    .find(|c| c.origin_id == copy.origin_id && c.seq == copy.seq)
                {
                    c.copies += copy.copies;
                }
                continue;
            }
            events.push(CommsEvent {
                kind: EventKind::Bundle,
                from: NodeKey::Balloon(ctx.balloons[from].id),
                to: NodeKey::Balloon(to),
                payload: 1,
                tower_id: None,
                hop_count: None,
                epoch: None,
            });
            self.stats.handoffs += 1;
            self.nodes[to_idx].held.push_back(copy);
        }

        // 4. Originate.
        for &i in &awake {
            let id = ctx.balloons[i].id;
            let node = &mut self.nodes[i];
            if round < node.next_bundle_round
                || node.held.len() >= self.params.relay_queue_capacity
            {
                continue;
            }
            if node.held.iter().any(|c| c.origin_id == id) {
                continue;
            }
            let record = TelemetryRecord::sample(&ctx.balloons[i], node.bundle_seq, round);
            if node.log.len() >= self.params.comms_log_capacity {
                node.log.pop_front();
            }
            node.log.push_back(record.clone());
            node.held.push_back(Copy_ {
                origin_id: id,
                seq: node.bundle_seq,
                created_at_round: round,
                record,
                copies: self.params.copies,
            });
            node.outstanding = Some(OutstandingBundle {
                seq: node.bundle_seq,
                created_at_round: round,
                state: AckState::Pending,
                path: None,
                channel: None,
                tower_id: None,
                ack_hops_completed: None,
            });
            node.bundle_seq += 1;
            node.next_bundle_round = round + self.params.bundle_interval_rounds;
            self.stats.originated += 1;
        }

        events
    }

    fn node_view(&self, i: usize) -> NodeCommsView {
        NodeCommsView {
            // Permanently None: there is no route to have an opinion about.
            route_hops: None,
            last_channel: self.nodes.get(i).and_then(|n| n.last_channel),
        }
    }

    fn carrying(&self, visible: usize) -> u64 {
        self.nodes[..visible.min(self.nodes.len())].iter().map(|n| n.held.len() as u64).sum()
    }

    /// Nothing is ever "stranded for want of a route" here, because routes
    /// don't exist. A copy in the wait phase with no tower in range is doing
    /// exactly what the protocol intends.
    fn stranded(&self, _visible: usize) -> u64 {
        0
    }

    fn delivered(&self) -> u64 {
        self.stats.delivered
    }

    /// The four canonical keys, plus what replication specifically costs.
    /// Nothing here about beliefs, next hops or acks — those are not zero
    /// under this protocol, they are absent.
    fn stats(&self) -> crate::protocol::stats::StatsTable {
        use crate::protocol::stats::StatsTable;
        let st = &self.stats;
        StatsTable::new()
            .count("originated", st.originated)
            .count("delivered", st.delivered)
            .count("resolved", st.resolved())
            .ratio("completion_rate", st.completion_rate())
            .count("satellite", st.satellite)
            .count("blocked", st.blocked)
            .count("duplicate_arrivals", st.duplicate_arrivals)
            .count("copies_expired", st.copies_expired)
            .count("handoffs", st.handoffs)
            .count("no_candidate", st.no_candidate)
            .ratio("handoffs_per_delivery", st.handoffs_per_delivery())
            // Keyed identically to dv-dtn's, so a harness can read the same
            // ratio off either protocol without knowing which it is driving.
            .count("unresolved", st.unresolved())
            .ratio("unresolved_share", st.unresolved_share())
            .ratio("delivered_per_originated", st.delivered_per_originated())
            .ratio("satellite_share", st.satellite_share())
            .ratio("delivery_latency_mean", st.delivery_latency.mean())
            .ratio("delivery_latency_p95", st.delivery_latency.percentile(0.95))
    }

    fn resolved(&self) -> u64 {
        self.stats.delivered + self.stats.satellite
    }

    /// No single path exists to replay, so the inspector's replay view has
    /// nothing to show — and says so by way of `next_hop_paths: false` rather
    /// than by returning something shaped like a path that isn't one.
    fn last_bundle(&self, _i: usize) -> Option<LastBundleView> {
        None
    }

    fn log(&self, i: usize) -> Vec<TelemetryRecord> {
        self.nodes.get(i).map(|n| n.log.iter().rev().cloned().collect()).unwrap_or_default()
    }

    fn halt_origination(&mut self) {
        for n in self.nodes.iter_mut() {
            n.next_bundle_round = u64::MAX;
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balloon::Balloon;
    use crate::link_detection::{Edge, NodeKey};
    use crate::mesh_adjacency::MeshAdjacency;
    use crate::tower::Tower;

    fn node_key(s: &str) -> NodeKey {
        let (tag, rest) = s.split_at(1);
        let id: u32 = rest.parse().unwrap();
        match tag {
            "b" => NodeKey::Balloon(id),
            "t" => NodeKey::Tower(id),
            _ => panic!("bad key"),
        }
    }
    fn edge(a: &str, b: &str) -> Edge {
        Edge { a: node_key(a), b: node_key(b) }
    }

    /// A chain t0 - b0 - b1 - b2 with only b2 originating. Nobody knows a
    /// route — there are none — so the record can only reach the ground by
    /// being copied along until some holder happens to be in tower range.
    fn chain(copies: u32) -> (Epidemic, Vec<Balloon>, Vec<Tower>, MeshAdjacency) {
        let p = EpidemicParams { copies, ..Default::default() };
        let mut e = Epidemic::with_params(p);
        e.reseed(11);
        let balloons: Vec<Balloon> =
            (0..3).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..3 {
            e.spawn_node();
        }
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2")], 3, &towers);
        (e, balloons, towers, adj)
    }

    fn run(e: &mut Epidemic, b: &[Balloon], t: &[Tower], adj: &MeshAdjacency, rounds: u64) {
        for round in 0..rounds {
            e.step(StepCtx { round, balloons: b, towers: t, adj });
        }
    }

    #[test]
    fn a_record_reaches_a_tower_by_replication_alone() {
        let (mut e, b, t, adj) = chain(8);
        // Only b2 originates, so delivery cannot be an accident of the origin
        // itself sitting next to the tower.
        e.nodes[0].next_bundle_round = u64::MAX;
        e.nodes[1].next_bundle_round = u64::MAX;
        run(&mut e, &b, &t, &adj, 400);
        assert!(e.stats.delivered >= 1, "stats: {:?}", e.stats);
        assert!(e.stats.handoffs >= 1, "it had to be copied to get there");
    }

    /// The property that makes spray-and-wait bounded: halving on each handoff
    /// means a record exists in at most `copies` instances network-wide, so no
    /// per-node record of everything ever seen is needed to stop re-infection.
    #[test]
    fn the_copy_budget_is_conserved() {
        let (mut e, b, t, adj) = chain(8);
        for round in 0..300u64 {
            e.step(StepCtx { round, balloons: &b, towers: &t, adj: &adj });
            let mut by_record: std::collections::HashMap<(u32, u64), u32> = Default::default();
            for n in &e.nodes {
                for c in &n.held {
                    *by_record.entry((c.origin_id, c.seq)).or_insert(0) += c.copies;
                }
            }
            for (k, total) in by_record {
                assert!(total <= 8, "record {k:?} reached {total} copies at round {round}");
            }
        }
    }

    /// The capability that matters: this protocol has no notion of a route, so
    /// it must never publish a hop count. The UI reads exactly this to decide
    /// whether the belief overlay means anything.
    #[test]
    fn no_node_ever_claims_a_route() {
        let (mut e, b, t, adj) = chain(8);
        assert!(!e.capabilities().route_belief);
        assert!(!e.capabilities().acks);
        run(&mut e, &b, &t, &adj, 300);
        for i in 0..e.nodes.len() {
            assert_eq!(e.node_view(i).route_hops, None);
        }
    }

    /// Server truth and the origin's own view diverge permanently here: nothing
    /// carries a receipt back, so a balloon whose record was delivered long ago
    /// still times out. This is the belief-vs-truth gap at its widest, and it
    /// is a property of the protocol rather than a gap in the model.
    #[test]
    fn an_origin_never_learns_its_record_landed() {
        let (mut e, b, t, adj) = chain(8);
        e.nodes[0].next_bundle_round = u64::MAX;
        e.nodes[1].next_bundle_round = u64::MAX;
        // Pin it to a single bundle: let one be originated and delivered,
        // then stop origination so `outstanding` can't roll on to a fresher
        // one that simply hasn't timed out yet.
        let mut round = 0u64;
        while round < 400 && e.stats.delivered == 0 {
            e.step(StepCtx { round, balloons: &b, towers: &t, adj: &adj });
            round += 1;
        }
        assert!(e.stats.delivered >= 1, "precondition: something was delivered");
        let delivered_seq = e.nodes[2].last_resolved.as_ref().expect("recorded").seq;
        e.halt_origination();
        for r in round..(round + 2 * e.params.bundle_max_age_rounds) {
            e.step(StepCtx { round: r, balloons: &b, towers: &t, adj: &adj });
        }

        let origin = &e.nodes[2];
        let out = origin.outstanding.as_ref().expect("originated something");
        assert_eq!(out.seq, delivered_seq, "still the bundle we watched land");
        assert_eq!(
            out.state,
            AckState::TimedOut,
            "the origin cannot know it landed, so it must time out"
        );
        // Server truth, meanwhile, recorded the radio delivery all along.
        assert_eq!(origin.last_channel, Some(Channel::Radio));
    }
}
