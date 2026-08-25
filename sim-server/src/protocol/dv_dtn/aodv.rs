// Reactive route discovery, AODV-style: find a route when something needs
// one, rather than maintaining one continuously.
//
// **Why this lives inside dv_dtn rather than beside it.** AODV and the shipped
// protocol share their entire forwarding half — bundles are carried hop by hop
// along a believed next hop, with the same queues, the same hold-don't-drop
// rule, the same acks and the same satellite fallback. Only *how the route is
// learned* differs. Making this a separate `MeshProtocol` would have meant
// duplicating 1400 lines of bundle.rs to change 300 lines of discovery, and
// then maintaining both. The family is "unicast store-and-forward along a
// believed next hop"; proactive-versus-reactive is a variant within it.
//
// The route it installs is a plain `RouteBelief`, which is why bundle.rs needs
// no changes at all: a route is a route, however it was found.
//
// The mechanism, on the same duty cycle everything else runs on:
//
//   1. A balloon holding a bundle with no live route broadcasts a **route
//      request** on its wake slot, tagged with its own id and a request
//      number.
//   2. A balloon hearing a request it hasn't seen records who it heard it
//      from — a *reverse pointer* back toward the requester — and either
//      answers it (if it can already reach a tower) or rebroadcasts it on its
//      own next wake slot.
//   3. A **route reply** travels back along those reverse pointers, one hop
//      per wake slot. Every balloon it passes through installs a forward route
//      whose next hop is whoever handed it the reply.
//
// The interesting contrast with the proactive protocol is *when the cost is
// paid*. Beacons spend airtime continuously whether or not anyone has data to
// send, and a balloon's route is usually already there but may be stale.
// Reactive discovery spends nothing until a bundle exists, then pays a
// round-trip flood before the first hop can move — and under a duty cycle
// that latency is hops × the wake interval, in each direction.

use super::beacon::RouteBelief;
use super::params::{DvDtnParams, ReplyOverhearing, ReplyPolicy, RingSearch};
use super::DvNode;
use crate::link_detection::NodeKey;
use crate::mesh_adjacency::MeshAdjacency;
use crate::protocol::{CommsEvent, EventKind};
use std::collections::HashMap;

/// One node's reactive-discovery state. Held separately from `DvNode` rather
/// than as fields on it, so the proactive mode carries none of it.
#[derive(Debug, Clone, Default)]
pub struct AodvNode {
    /// Requests already seen, mapped to the neighbour they arrived from —
    /// the reverse path a reply will travel. Keyed by (requester, request id)
    /// so a re-issued request is a distinct flood.
    pub reverse: HashMap<(u32, u64), u32>,
    /// Requests waiting to be rebroadcast on this node's next wake slot.
    pub to_forward: Vec<Rreq>,
    /// Replies waiting to be relayed back toward a requester.
    pub replies: Vec<Rrep>,
    /// Hop budget for this node's *next* request. Under
    /// `RingSearch::Expanding` it doubles each time an attempt goes unanswered
    /// and resets once a route is found; under `RingSearch::Max` it is unused.
    /// Zero means "not started", which the issuing path clamps up to one.
    pub next_ttl: u32,
    /// This node's own outstanding request, if it is looking for a route.
    pub pending: Option<Pending>,
    pub next_rreq_id: u64,
}

/// A request this node has out and is waiting on.
#[derive(Debug, Clone, Copy)]
pub struct Pending {
    pub id: u64,
    pub issued_at: u64,
    /// How far this attempt was allowed to travel. Under
    /// `RingSearch::Expanding` the next attempt doubles it.
    pub ttl: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Rreq {
    pub requester: u32,
    pub id: u64,
    pub hops: u32,
    /// The requester's hop budget for *this* attempt, carried on the request so
    /// every rebroadcaster enforces the same ring without needing to be told
    /// separately. Under `RingSearch::Max` this is always `beacon_max_hops`.
    pub ttl: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Rrep {
    pub requester: u32,
    pub id: u64,
    pub tower_id: u32,
    /// Hops from the *replying* node to the tower; incremented as the reply
    /// travels back, so each node installs its own true distance.
    pub hops: u32,
    /// When the answering node's knowledge was current. Copied verbatim as the
    /// reply travels, exactly as a beacon's emission round is — so a route's
    /// age means the same thing in both modes and expiry is unchanged.
    pub emitted_at_round: u64,
}

#[derive(Debug, Default)]
pub struct State {
    pub nodes: Vec<AodvNode>,
}

impl State {
    pub fn resize(&mut self, n: usize) {
        self.nodes.resize(n, AodvNode::default());
    }
}

/// How long a requester waits before concluding an attempt failed.
///
/// Under a fixed maximum hop budget this is just the belief lease, which is
/// what it has always been. Under an expanding ring it has to scale with the
/// ring, because a request travels **one hop per wake slot** in each direction:
/// a ring of `ttl` hops cannot possibly be answered in less than `2 * ttl`
/// slots, so a fixed timeout would abandon wide rings before they could return
/// and the search would never get past the narrow ones.
fn attempt_timeout(params: &DvDtnParams, ttl: u32) -> u64 {
    match params.ring_search {
        RingSearch::Max => params.belief_max_age_rounds,
        RingSearch::Expanding => {
            (2 * ttl as u64 + 1).saturating_mul(params.beacon_interval_rounds)
        }
    }
}

/// One round of reactive discovery. Mirrors `beacon::step`'s contract: expires
/// stale routes, decides who transmits, and returns the wake set that bundle
/// forwarding will reuse.
pub fn step(
    nodes: &mut [DvNode],
    state: &mut State,
    balloons: &[crate::balloon::Balloon],
    adj: &MeshAdjacency,
    params: &DvDtnParams,
    round: u64,
    rng: &mut (impl rand::Rng + ?Sized),
) -> (Vec<usize>, Vec<CommsEvent>) {
    let n = nodes.len();
    state.resize(n);
    let mut events = Vec::new();

    // 1. Expire routes on the same rule the proactive mode uses: age of the
    //    news, not recency of hearing it.
    for node in nodes.iter_mut() {
        if node.belief.is_some_and(|b| b.is_expired(round, params.belief_max_age_rounds)) {
            node.belief = None;
        }
    }
    // Forget stale reverse pointers and abandoned requests, or the tables grow
    // without bound over a long run. Under an expanding ring this is also where
    // the ring grows: an attempt that timed out becomes the next, wider one.
    for a in state.nodes.iter_mut() {
        if let Some(p) = a.pending {
            if round.saturating_sub(p.issued_at) > attempt_timeout(params, p.ttl) {
                a.pending = None;
                a.next_ttl = (p.ttl.saturating_mul(2)).min(params.beacon_max_hops);
            }
        }
    }

    // 2. Wake set — same duty cycle as everything else.
    let mut awake = Vec::new();
    for (i, node) in nodes.iter_mut().enumerate().take(n) {
        if round < node.next_beacon_round {
            continue;
        }
        node.next_beacon_round = super::beacon::next_slot(round, params, rng);
        awake.push(i);
    }

    // 3. Gather. Two-phase for the same reason as everywhere else: applying in
    //    place would let one flood cross the mesh within a single round.
    let mut rreq_out: Vec<(usize, Rreq)> = Vec::new(); // (sender index, request)
    let mut rrep_out: Vec<(usize, u32, Rrep)> = Vec::new(); // (sender, to, reply)

    for &i in &awake {
        let id = balloons[i].id;

        // Replies first: a reply in hand is someone else's blocked bundle, and
        // the same reasoning that gives acks priority applies.
        if let Some(reply) = state.nodes[i].replies.pop() {
            if reply.requester == id {
                continue; // it is ours; it was consumed on arrival
            }
            if let Some(&back) = state.nodes[i].reverse.get(&(reply.requester, reply.id)) {
                if adj.is_neighbor(i, back) {
                    rrep_out.push((i, back, reply));
                    continue;
                }
            }
            continue; // reverse path gone — the requester will retry
        }

        // Then rebroadcasts. The ring the requester asked for is enforced by
        // every rebroadcaster, not just by the requester — which is the whole
        // point of carrying `ttl` on the request rather than keeping it at the
        // origin.
        if let Some(req) = state.nodes[i].to_forward.pop() {
            // `ttl` counts hops the request may *travel*, so a request already
            // `hops` out may only be passed on if the extra hop stays inside
            // the budget. Comparing `hops < ttl` instead would let a ring of 1
            // reach two hops out, which is not a ring of 1.
            if req.hops + 1 < req.ttl.min(params.beacon_max_hops) {
                rreq_out.push((i, Rreq { hops: req.hops + 1, ..req }));
            }
            continue;
        }

        // Then, if this balloon has something to send and nowhere to send it,
        // it asks. One outstanding request at a time: a flood per wake slot
        // per balloon would swamp the mesh it is trying to measure.
        let needs_route = !nodes[i].queue.is_empty() && nodes[i].belief.is_none();
        if needs_route && state.nodes[i].pending.is_none() {
            let ttl = match params.ring_search {
                RingSearch::Max => params.beacon_max_hops,
                RingSearch::Expanding => {
                    state.nodes[i].next_ttl.clamp(1, params.beacon_max_hops)
                }
            };
            let rid = state.nodes[i].next_rreq_id;
            state.nodes[i].next_rreq_id += 1;
            state.nodes[i].pending = Some(Pending { id: rid, issued_at: round, ttl });
            rreq_out.push((i, Rreq { requester: id, id: rid, hops: 0, ttl }));
        }
    }

    // 4. Apply requests: every neighbour in range hears the broadcast.
    for (from_idx, req) in rreq_out {
        let from_id = balloons[from_idx].id;
        for &nb in adj.neighbors(from_idx) {
            let nb_idx = nb as usize;
            if nb_idx >= n || nb == req.requester {
                continue;
            }
            events.push(CommsEvent {
                kind: EventKind::RouteRequest,
                from: NodeKey::Balloon(from_id),
                to: NodeKey::Balloon(nb),
                payload: 1,
                tower_id: None,
                hop_count: Some(req.hops),
                epoch: Some(req.id),
            });
            let key = (req.requester, req.id);
            if state.nodes[nb_idx].reverse.contains_key(&key) {
                continue; // already flooded through here
            }
            state.nodes[nb_idx].reverse.insert(key, from_id);

            // Can this balloon answer? Either it can hear a tower itself, or
            // it already holds a live route to one.
            //
            // A reply carries the *answering* node's own distance to the
            // tower; the +1 for the final hop is added by whoever receives it
            // (see below). Adding it at both ends inflates every route by one
            // and gets worse each time a reply is relayed.
            //
            // It also carries the age of the knowledge it was built on, not
            // the moment it was sent. Only a node hearing a tower right now
            // may stamp the current round. **This is the anti-laundering rule
            // the proactive side already enforces**, and skipping it here was
            // a bug rather than a simplification: with freshness-first
            // adoption, a node answering from a 40-round-old 12-hop route
            // restamped it as news-of-this-instant, and it then beat a
            // genuinely current 2-hop reply arriving alongside it. Routes
            // inflated instead of converging, which is what real AODV's
            // destination sequence numbers exist to prevent.
            let answer = if let Some(tower_id) = adj.tower_in_range(nb_idx) {
                Some((tower_id, 1u32, round)) // adjacent: one hop, first-hand
            } else if params.reply_policy == ReplyPolicy::Intermediate {
                nodes[nb_idx].belief.map(|b| (b.tower_id, b.hop_count, b.emitted_at_round))
            } else {
                None
            };
            match answer {
                Some((tower_id, hops, emitted_at_round)) => {
                    state.nodes[nb_idx].replies.push(Rrep {
                        requester: req.requester,
                        id: req.id,
                        tower_id,
                        hops,
                        emitted_at_round,
                    })
                }
                None => state.nodes[nb_idx].to_forward.push(req),
            }
        }
    }

    // 5. Apply replies. Each hop back installs a route whose next hop is
    //    whoever handed the reply over — that node is one step closer.
    for (from_idx, to, reply) in rrep_out {
        let to_idx = to as usize;
        if to_idx >= n {
            continue;
        }
        let from_id = balloons[from_idx].id;
        events.push(CommsEvent {
            kind: EventKind::RouteReply,
            from: NodeKey::Balloon(from_id),
            to: NodeKey::Balloon(to),
            payload: 1,
            tower_id: Some(reply.tower_id),
            hop_count: Some(reply.hops),
            epoch: Some(reply.id),
        });
        let installed = RouteBelief {
            tower_id: reply.tower_id,
            hop_count: reply.hops + 1,
            next_hop: Some(from_id),
            epoch: reply.id,
            emitted_at_round: reply.emitted_at_round,
        };
        if super::beacon::should_adopt(nodes[to_idx].belief.as_ref(), &installed, params) {
            nodes[to_idx].belief = Some(installed);
        }
        if balloons[to_idx].id == reply.requester {
            state.nodes[to_idx].pending = None; // answered
            state.nodes[to_idx].next_ttl = 1; // a ring that worked starts small again
        } else {
            state.nodes[to_idx].replies.push(Rrep { hops: reply.hops + 1, ..reply });
        }

        // Everyone else within earshot of the transmitter learns the same route
        // on the same transmission. No event is emitted for these: physically
        // this *is* the one transmission already recorded above, and counting
        // it once per listener would misreport the airtime the mechanism costs
        // — which is none, and is the entire argument for it.
        if params.reply_overhearing == ReplyOverhearing::On {
            for &nb in adj.neighbors(from_idx) {
                let nb_idx = nb as usize;
                if nb_idx >= n || nb == to {
                    continue;
                }
                // **An overhearer adopts only on improvement, never merely on
                // freshness** — which is the opposite of the rule the addressee
                // uses, deliberately.
                //
                // The addressee asked: the reply is an answer to its own
                // question, and it is on the reverse path, so the route is
                // about it. An overhearer has no such standing. It is picking
                // up a route computed for somebody else, and the transmitter's
                // own path may well run back through the overhearer — adopting
                // that produces a two-node loop out of nothing.
                //
                // Measured before this gate existed: overhearing cut
                // `stall_no_belief` by 65% and satellite fallback by 59%,
                // exactly as intended, and then gave all of it back as
                // `dropped_loop` (131 -> 610) while mean believed depth *grew*
                // 5.67 -> 7.26 hops. Freshness-first is the right rule for an
                // answer and the wrong one for a rumour.
                //
                // Those are pre-gate numbers from a single run, and are *not*
                // the figures the docs quote: with the gate in place the
                // committed sweep gives 131 -> 679 (discovery-sweep-results.csv,
                // 20 seeds). Same experiment, different code — don't reconcile
                // them.
                let better = match nodes[nb_idx].belief {
                    None => true,
                    Some(cur) => {
                        cur.is_expired(round, params.belief_max_age_rounds)
                            || installed.hop_count < cur.hop_count
                    }
                };
                if better {
                    nodes[nb_idx].belief = Some(installed);
                    // An overhearer with a route no longer needs the one it
                    // asked for; dropping the request stops it re-flooding on
                    // its next slot.
                    state.nodes[nb_idx].pending = None;
                    state.nodes[nb_idx].next_ttl = 1;
                }
            }
        }
    }

    // A balloon that can hear a tower needs no discovery at all — it is
    // already at the destination. Recorded directly so the last hop behaves
    // identically in both modes.
    for (i, node) in nodes.iter_mut().enumerate().take(n) {
        if let Some(tower_id) = adj.tower_in_range(i) {
            let direct = RouteBelief {
                tower_id,
                hop_count: 1,
                next_hop: None,
                epoch: 0,
                emitted_at_round: round,
            };
            if super::beacon::should_adopt(node.belief.as_ref(), &direct, params) {
                node.belief = Some(direct);
            }
        }
    }

    (awake, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balloon::Balloon;
    use crate::link_detection::Edge;
    use crate::protocol::dv_dtn::params::Discovery;
    use crate::protocol::{MeshProtocol, ProtocolSpec, StepCtx};
    use crate::tower::Tower;

    fn key(s: &str) -> NodeKey {
        let (t, r) = s.split_at(1);
        let id: u32 = r.parse().unwrap();
        if t == "b" { NodeKey::Balloon(id) } else { NodeKey::Tower(id) }
    }
    fn edge(a: &str, b: &str) -> Edge {
        Edge { a: key(a), b: key(b) }
    }

    /// t0 - b0 - b1 - b2. Only b2 originates, so the route has to be
    /// discovered across two hops and answered back across two.
    #[test]
    fn a_route_is_discovered_on_demand_and_a_bundle_follows_it() {
        let p = DvDtnParams {
            discovery: Discovery::Reactive,
            ..Default::default()
        };
        let mut d = super::super::DvDtn::with_params(p);
        d.reseed(5);
        let balloons: Vec<Balloon> =
            (0..3).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..3 {
            d.spawn_node();
        }
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2")], 3, &towers);

        // Nobody has a route to begin with: nothing has been asked for yet.
        assert!(d.nodes.iter().all(|n| n.belief.is_none()));

        for round in 0..400u64 {
            d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
        }

        // b2 is two hops out and must have learned a route pointing at b1.
        let b2 = d.nodes[2].belief.expect("b2 should have discovered a route");
        assert_eq!(b2.next_hop, Some(1), "next hop must point back the way the reply came");
        assert_eq!(b2.hop_count, 3);
        assert!(d.stats.delivered >= 1, "and bundles should then flow: {:?}", d.stats);
    }

    /// Discovery is *reactive*: with nothing to send, nothing is asked, and no
    /// route appears. This is the property that distinguishes it from the
    /// proactive mode, where a route shows up whether or not it is wanted.
    #[test]
    fn no_bundle_means_no_discovery_traffic() {
        let p = DvDtnParams {
            discovery: Discovery::Reactive,
            ..Default::default()
        };
        let mut d = super::super::DvDtn::with_params(p);
        d.reseed(5);
        let balloons: Vec<Balloon> =
            (0..3).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..3 {
            d.spawn_node();
        }
        // b1 and b2 never originate and cannot hear a tower, so they have no
        // reason to look for one.
        for n in d.nodes.iter_mut() {
            n.next_bundle_round = u64::MAX;
        }
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2")], 3, &towers);

        let mut requests = 0;
        for round in 0..200u64 {
            let ev = d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
            requests += ev.iter().filter(|e| e.kind == EventKind::RouteRequest).count();
        }
        assert_eq!(requests, 0, "nothing to send, so nothing should be asked");
        assert!(d.nodes[2].belief.is_none(), "and b2 should still know no route");
    }

    /// A node answering from its own route must pass on the age of the news it
    /// holds, not the moment it happened to answer. Restamping is *laundering*:
    /// it turns second-hand knowledge into apparent first-hand knowledge, and
    /// with freshness-first adoption that stale-but-fresh-looking route then
    /// beats a genuinely current one. Route lengths inflate instead of
    /// converging, and beliefs stop draining when the towers go away.
    ///
    /// This is exactly the invariant the proactive mode gets by relaying
    /// `emitted_at_round` verbatim, and it was broken here until replies
    /// started carrying it. Real AODV solves the same problem with destination
    /// sequence numbers.
    ///
    /// b0 - b1, with neither in range of a tower. b1 already holds an old
    /// route; b0 has a bundle and no route, so it asks and b1 answers.
    #[test]
    fn an_answer_from_a_stale_route_does_not_pass_it_off_as_current() {
        const NEWS: u64 = 40;
        let p = DvDtnParams {
            discovery: Discovery::Reactive,
            ..Default::default()
        };
        let mut d = super::super::DvDtn::with_params(p);
        d.reseed(5);
        let balloons: Vec<Balloon> =
            (0..2).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..2 {
            d.spawn_node();
        }
        // No tower edge: the only route in the system is the one b1 is holding,
        // so whatever b0 ends up with came from b1's answer.
        let towers = vec![Tower::new(0, -50.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("b0", "b1")], 2, &towers);

        d.nodes[1].belief = Some(RouteBelief {
            tower_id: 0,
            hop_count: 3,
            next_hop: None,
            epoch: 1,
            emitted_at_round: NEWS,
        });
        d.nodes[1].next_bundle_round = u64::MAX; // only b0 originates, so only b0 asks
        d.nodes[0].next_bundle_round = NEWS;

        for round in NEWS..(NEWS + 50) {
            d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
        }

        let b0 = d.nodes[0].belief.expect("b0 should have been answered by b1");
        assert_eq!(b0.hop_count, 4, "b1 is 3 hops out, so b0 is 4");
        assert_eq!(
            b0.emitted_at_round, NEWS,
            "b0's route must carry the age of b1's news ({NEWS}), not the round b1 \
             answered on — restamping it would make a stale route immortal"
        );
    }

    /// Tower-adjacent gating is the strict version: no node may answer from
    /// second-hand knowledge, so every route is as short as the flood that
    /// found it. Cheap to state, and it keeps the parameter honest.
    #[test]
    fn tower_adjacent_gating_still_finds_the_route() {
        let p = DvDtnParams {
            discovery: Discovery::Reactive,
            reply_policy: ReplyPolicy::TowerAdjacent,
            ..Default::default()
        };
        let mut d = super::super::DvDtn::with_params(p);
        d.reseed(5);
        let balloons: Vec<Balloon> =
            (0..3).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..3 {
            d.spawn_node();
        }
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2")], 3, &towers);

        for round in 0..400u64 {
            d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
        }
        let b2 = d.nodes[2].belief.expect("b2 should still discover a route");
        assert_eq!(b2.next_hop, Some(1));
        assert_eq!(b2.hop_count, 3, "and it must be the true distance, not an inflated one");
    }

    /// A reply is a radio transmission, so everyone in earshot hears it — not
    /// only the node on the reverse path. b3 never asked for anything and is
    /// not on b2's reverse path, but it is a neighbour of b1, so when b1 hands
    /// the reply to b2 it learns the route too.
    ///
    /// The route it picks up must be the same one b2 installs, including the
    /// unrestamped emission round: overhearing may not become a second way to
    /// launder a stale route.
    #[test]
    fn a_neighbour_overhearing_a_reply_installs_the_route_too() {
        let p = DvDtnParams {
            discovery: Discovery::Reactive,
            reply_overhearing: ReplyOverhearing::On,
            ..Default::default()
        };
        let mut d = super::super::DvDtn::with_params(p);
        d.reseed(5);
        let balloons: Vec<Balloon> =
            (0..4).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..4 {
            d.spawn_node();
        }
        // t0 - b0 - b1 - b2, with b3 also hanging off b1 but never originating,
        // so it has no reason of its own to look for a route.
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(
            &[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2"), edge("b1", "b3")],
            4,
            &towers,
        );
        d.nodes[3].next_bundle_round = u64::MAX;

        for round in 0..400u64 {
            d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
        }

        let b3 = d.nodes[3].belief.expect("b3 should have learned from a reply meant for b2");
        assert_eq!(b3.next_hop, Some(1), "it heard the route from b1, so b1 is the way out");
        assert_eq!(b3.hop_count, 3, "the same distance b2 installs, not an inflated one");
    }

    /// With overhearing off, the same b3 stays ignorant — which is what makes
    /// the test above a measurement of the mechanism rather than of the layout.
    #[test]
    fn without_overhearing_a_silent_neighbour_learns_nothing() {
        let p = DvDtnParams {
            discovery: Discovery::Reactive,
            reply_overhearing: ReplyOverhearing::Off,
            ..Default::default()
        };
        let mut d = super::super::DvDtn::with_params(p);
        d.reseed(5);
        let balloons: Vec<Balloon> =
            (0..4).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..4 {
            d.spawn_node();
        }
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(
            &[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2"), edge("b1", "b3")],
            4,
            &towers,
        );
        d.nodes[3].next_bundle_round = u64::MAX;

        for round in 0..400u64 {
            d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
        }
        assert!(
            d.nodes[3].belief.is_none(),
            "b3 asked for nothing and was told nothing: {:?}",
            d.nodes[3].belief
        );
    }

    /// `ttl` is how far a request may *travel*, so a ring of 1 reaches direct
    /// neighbours and stops. Pinned on its own because the off-by-one is
    /// invisible end-to-end — a ring one hop too wide still finds the route,
    /// just without being the ring it claims to be, so every measurement of
    /// "cost of a ring" would be quietly attributed to the wrong ring.
    #[test]
    fn a_ring_of_one_reaches_direct_neighbours_and_stops() {
        let p = DvDtnParams {
            discovery: Discovery::Reactive,
            ring_search: RingSearch::Expanding,
            reply_policy: ReplyPolicy::TowerAdjacent,
            ..Default::default()
        };
        let mut d = super::super::DvDtn::with_params(p);
        d.reseed(5);
        let balloons: Vec<Balloon> =
            (0..4).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..4 {
            d.spawn_node();
        }
        // t0 - b0 - b1 - b2 - b3, a straight chain: b3's only answer is b0,
        // three hops away.
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(
            &[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2"), edge("b2", "b3")],
            4,
            &towers,
        );
        for i in 0..3 {
            d.nodes[i].next_bundle_round = u64::MAX;
        }
        d.nodes[3].next_bundle_round = 0;

        // Through the whole of the first ring's timeout, the request may only
        // ever have been heard by b2 — which cannot answer — so no reply can
        // exist and nothing may travel back.
        let first_ring = attempt_timeout(&d.params, 1);
        let mut replies = 0;
        for round in 0..=first_ring {
            let ev = d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
            replies += ev.iter().filter(|e| e.kind == EventKind::RouteReply).count();
        }
        assert_eq!(replies, 0, "a one-hop ring reached something two hops out");
        assert!(d.nodes[3].belief.is_none());
    }

    /// And the ring has to actually widen, or the search would stall at one hop
    /// forever. Same chain; b3 keeps originating, so it keeps asking, and the
    /// route must eventually come back.
    #[test]
    fn an_expanding_ring_widens_until_it_finds_the_route() {
        let p = DvDtnParams {
            discovery: Discovery::Reactive,
            ring_search: RingSearch::Expanding,
            reply_policy: ReplyPolicy::TowerAdjacent,
            ..Default::default()
        };
        let mut d = super::super::DvDtn::with_params(p);
        d.reseed(5);
        let balloons: Vec<Balloon> =
            (0..4).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..4 {
            d.spawn_node();
        }
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(
            &[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2"), edge("b2", "b3")],
            4,
            &towers,
        );
        for i in 0..3 {
            d.nodes[i].next_bundle_round = u64::MAX;
        }
        d.nodes[3].next_bundle_round = 0;

        // Checked over the run rather than at the end: a reactive balloon that
        // has nothing left to send correctly lets its route expire, so the
        // final state is "no route" whether or not discovery ever worked.
        let mut found = None;
        for round in 0..600u64 {
            d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
            if found.is_none() {
                found = d.nodes[3].belief.map(|b| (round, b.hop_count));
            }
        }
        let (round, hops) = found.expect("the ring must widen until it reaches b0");
        assert_eq!(hops, 4, "three balloon hops plus the ground hop");
        assert!(
            round > attempt_timeout(&d.params, 1),
            "found at round {round}, inside the first ring — the ring never widened"
        );
    }

    /// The digest rides tower beacons, which reactive discovery doesn't send.
    /// Silently never acknowledging would look like a protocol result rather
    /// than a missing mechanism, so the combination is refused up front.
    #[test]
    fn reactive_discovery_refuses_the_beacon_borne_ack_digest() {
        let err = "dv-dtn:discovery=reactive,ack=digest".parse::<ProtocolSpec>().unwrap_err();
        assert!(err.contains("tower beacons"), "unhelpful error: {err}");
        assert!("dv-dtn:discovery=reactive".parse::<ProtocolSpec>().is_ok());
    }
}

