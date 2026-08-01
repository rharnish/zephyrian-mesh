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
use super::params::DvDtnParams;
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
    /// This node's own outstanding request, if it is looking for a route.
    pub pending: Option<(u64, u64)>, // (request id, issued at round)
    pub next_rreq_id: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Rreq {
    pub requester: u32,
    pub id: u64,
    pub hops: u32,
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
    // without bound over a long run.
    for a in state.nodes.iter_mut() {
        if let Some((_, issued)) = a.pending {
            if round.saturating_sub(issued) > params.belief_max_age_rounds {
                a.pending = None;
            }
        }
    }

    // 2. Wake set — same duty cycle as everything else.
    let mut awake = Vec::new();
    for i in 0..n {
        if round < nodes[i].next_beacon_round {
            continue;
        }
        nodes[i].next_beacon_round = super::beacon::next_slot(round, params, rng);
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

        // Then rebroadcasts.
        if let Some(req) = state.nodes[i].to_forward.pop() {
            if req.hops < params.beacon_max_hops {
                rreq_out.push((i, Rreq { hops: req.hops + 1, ..req }));
            }
            continue;
        }

        // Then, if this balloon has something to send and nowhere to send it,
        // it asks. One outstanding request at a time: a flood per wake slot
        // per balloon would swamp the mesh it is trying to measure.
        let needs_route = !nodes[i].queue.is_empty() && nodes[i].belief.is_none();
        if needs_route && state.nodes[i].pending.is_none() {
            let rid = state.nodes[i].next_rreq_id;
            state.nodes[i].next_rreq_id += 1;
            state.nodes[i].pending = Some((rid, round));
            rreq_out.push((i, Rreq { requester: id, id: rid, hops: 0 }));
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
            // A reply carries the *answering* node's own distance to the
            // tower; the +1 for the final hop is added by whoever receives it
            // (see below). Adding it at both ends inflates every route by one
            // and gets worse each time a reply is relayed.
            let answer = if let Some(tower_id) = adj.tower_in_range(nb_idx) {
                Some((tower_id, 1u32)) // adjacent: one hop from the ground
            } else {
                nodes[nb_idx].belief.map(|b| (b.tower_id, b.hop_count))
            };
            match answer {
                Some((tower_id, hops)) => state.nodes[nb_idx].replies.push(Rrep {
                    requester: req.requester,
                    id: req.id,
                    tower_id,
                    hops,
                    emitted_at_round: round,
                }),
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
        } else {
            state.nodes[to_idx].replies.push(Rrep { hops: reply.hops + 1, ..reply });
        }
    }

    // A balloon that can hear a tower needs no discovery at all — it is
    // already at the destination. Recorded directly so the last hop behaves
    // identically in both modes.
    for i in 0..n {
        if let Some(tower_id) = adj.tower_in_range(i) {
            let direct = RouteBelief {
                tower_id,
                hop_count: 1,
                next_hop: None,
                epoch: 0,
                emitted_at_round: round,
            };
            if super::beacon::should_adopt(nodes[i].belief.as_ref(), &direct, params) {
                nodes[i].belief = Some(direct);
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
        let mut p = DvDtnParams::default();
        p.discovery = Discovery::Reactive;
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
        let mut p = DvDtnParams::default();
        p.discovery = Discovery::Reactive;
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
