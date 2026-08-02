// Gossiped link-state: balloons exchange *who they can hear*, and each one
// works out its own route from the picture it has assembled.
//
// The third discovery variant, and the one that differs most from the other
// two while still ending at the same place — a `RouteBelief` with a next hop,
// which is why bundle.rs again needs no changes.
//
// **What moves on the wire.** The other two variants circulate *conclusions*:
// a beacon says "a tower is 4 hops away through me", a route reply says "I can
// reach one". Here a balloon broadcasts only observations — its current
// neighbour list, and whether it can hear a tower — and never an opinion about
// distance. Each balloon accumulates other balloons' observations into a small
// map of the mesh and runs its own breadth-first search over it.
//
// **Why that is worth having in this project specifically.** Distance-vector
// belief is thin: one route, one hop count, no way to tell a route that just
// broke from one that is merely quiet. A link-state balloon holds a whole map
// and can therefore be *wrong in more interesting ways* — its map can be
// internally consistent, recently updated, and still describe a mesh that no
// longer exists. That is a sharper version of the belief-versus-truth gap the
// simulator is for, and it is not reachable with a hop count.
//
// **Freshness of a computed route.** A route is only as current as the
// stalest link it depends on, so the installed belief carries the *minimum*
// `emitted_at_round` over every record on the path. Routes therefore expire on
// the same soft-state rule as everywhere else, and a path assembled from one
// fresh observation and four old ones is correctly treated as old.
//
// **No `should_adopt` here, deliberately.** The other variants compare an
// incoming offer against what they hold, because an offer is all they get.
// A link-state node recomputes from its database and installs the answer,
// replacing whatever it believed — including replacing a route with *nothing*
// when the map no longer shows a path. Competing offers is a distance-vector
// idea; it does not belong on this side.
//
// The cost is airtime and memory rather than latency: every balloon carries a
// partial map of the fleet, and observations have to keep circulating for the
// maps to stay current.

use super::beacon::RouteBelief;
use super::params::DvDtnParams;
use super::DvNode;
use crate::link_detection::NodeKey;
use crate::mesh_adjacency::MeshAdjacency;
use crate::protocol::{CommsEvent, EventKind};
use std::collections::{HashMap, HashSet, VecDeque};

/// One balloon's observation of its own surroundings, as broadcast. This is
/// the only thing that travels: no distances, no next hops, no conclusions.
#[derive(Debug, Clone)]
pub struct Lsa {
    pub origin: u32,
    /// Sequence number from the originator, so a receiver can tell a newer
    /// observation from an older one it already holds. The link-state
    /// equivalent of the beacon epoch.
    pub seq: u64,
    /// When the *originator* observed this, never restamped by a relay — the
    /// same anti-laundering rule the rest of the family runs on.
    pub emitted_at_round: u64,
    pub neighbors: Vec<u32>,
    /// A tower this balloon can hand a bundle straight to, if any. The only
    /// thing that makes a node a destination worth routing toward.
    pub tower: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct LsNode {
    /// This balloon's map of the mesh, keyed by whose observation it is.
    /// Bounded in practice by expiry rather than by capacity: records age out
    /// on the same lease as beliefs.
    pub db: HashMap<u32, Lsa>,
    /// Sequence for this balloon's own observations.
    pub seq: u64,
    /// Origins whose records this balloon has learned but not yet passed on.
    /// A queue rather than a set so relaying is fair: an observation that
    /// arrived first goes out first, instead of one node's updates crowding
    /// out another's indefinitely.
    pub to_relay: VecDeque<u32>,
    /// Whether the map has changed since the route was last computed. Route
    /// computation is the expensive part, so it only runs when the input to it
    /// has actually moved.
    pub dirty: bool,
}

#[derive(Debug, Default)]
pub struct State {
    pub nodes: Vec<LsNode>,
    /// Records that arrived carrying nothing new — the receiver already held
    /// that origin at an equal or newer sequence. This is the headroom an
    /// MPR-style relay scheme would be competing for: every one of these
    /// consumed a slice of somebody's wake slot and taught nobody anything.
    pub records_redundant: u64,
    /// Records that did teach the receiver something.
    pub records_useful: u64,
}

impl State {
    pub fn resize(&mut self, n: usize) {
        self.nodes.resize(n, LsNode::default());
    }
}

/// One round of gossip. Same contract as `beacon::step` and `aodv::step`:
/// expire, decide who transmits, and hand back the wake set that bundle
/// forwarding rides on.
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

    // 1. Expire records on age of observation, not recency of hearing. A map
    //    that cannot be refreshed has to shrink, or balloons would route over
    //    links that stopped existing hours ago.
    for ls in state.nodes.iter_mut() {
        let before = ls.db.len();
        ls.db.retain(|_, r| round.saturating_sub(r.emitted_at_round) <= params.belief_max_age_rounds);
        if ls.db.len() != before {
            ls.dirty = true;
        }
    }

    // 2. Wake set — the same duty cycle as everything else.
    let mut awake = Vec::new();
    for (i, node) in nodes.iter_mut().enumerate().take(n) {
        if round < node.next_beacon_round {
            continue;
        }
        node.next_beacon_round = super::beacon::next_slot(round, params, rng);
        awake.push(i);
    }

    // 3. Gather. One wake slot is one transmission carrying a bounded batch of
    //    records: this balloon's own fresh observation, then as many relayed
    //    ones as the batch allows. Own-first matters — it is the only record
    //    nobody else can supply, and dropping it would leave a balloon
    //    invisible to the mesh while it forwarded gossip about others.
    let mut out: Vec<(usize, Vec<Lsa>)> = Vec::new();
    for &i in &awake {
        let id = balloons[i].id;
        let ls = &mut state.nodes[i];

        ls.seq += 1;
        let own = Lsa {
            origin: id,
            seq: ls.seq,
            emitted_at_round: round,
            neighbors: adj.neighbors(i).to_vec(),
            tower: adj.tower_in_range(i),
        };
        ls.db.insert(id, own.clone());
        ls.dirty = true;

        let mut batch = vec![own];
        while batch.len() < params.lsa_per_transmission.max(1) {
            let Some(origin) = ls.to_relay.pop_front() else { break };
            if origin == id {
                continue; // already carried, and always fresher than a relay
            }
            // It may have expired since it was queued; a relay is not a reason
            // to keep something alive past its lease.
            if let Some(r) = ls.db.get(&origin) {
                batch.push(r.clone());
            }
        }
        out.push((i, batch));
    }

    // 4. Apply. Every neighbour in range hears the whole batch — one
    //    transmission, several records, which is what `payload` on the event
    //    is for.
    let (mut redundant, mut useful) = (0u64, 0u64);
    for (from_idx, batch) in out {
        let from_id = balloons[from_idx].id;
        for &nb in adj.neighbors(from_idx) {
            let nb_idx = nb as usize;
            if nb_idx >= n {
                continue;
            }
            events.push(CommsEvent {
                kind: EventKind::Gossip,
                from: NodeKey::Balloon(from_id),
                to: NodeKey::Balloon(nb),
                payload: batch.len() as u32,
                tower_id: None,
                hop_count: None,
                epoch: Some(batch[0].seq),
            });
            let ls = &mut state.nodes[nb_idx];
            for rec in &batch {
                if rec.origin == nb {
                    continue; // a node's own observation, reflected back
                }
                // Newer observations win; equal or older ones are dropped
                // without being relayed, which is what stops the flood.
                let newer = ls.db.get(&rec.origin).is_none_or(|cur| rec.seq > cur.seq);
                if !newer {
                    redundant += 1;
                    continue;
                }
                useful += 1;
                ls.db.insert(rec.origin, rec.clone());
                ls.dirty = true;
                if !ls.to_relay.contains(&rec.origin) {
                    ls.to_relay.push_back(rec.origin);
                }
            }
        }
    }

    state.records_redundant += redundant;
    state.records_useful += useful;

    // 5. Recompute routes. Only for balloons that transmitted this round and
    //    whose map actually moved — the search is the expensive part of this
    //    variant, and running it for every balloon every round would dominate
    //    the simulation without changing any result.
    for &i in &awake {
        if !state.nodes[i].dirty {
            continue;
        }
        state.nodes[i].dirty = false;
        nodes[i].belief = shortest_route(&state.nodes[i].db, balloons[i].id, params);
    }

    (awake, events)
}

/// Breadth-first search from `self_id` to the nearest balloon that can hear a
/// tower, over whatever the database currently claims.
///
/// Returns `None` when the map shows no path — and that `None` is installed,
/// replacing any previous belief. A link-state node that can no longer see a
/// route to the ground *knows* it, which is a stronger statement than the
/// distance-vector variants can make: there, the same situation is
/// indistinguishable from not having heard anything lately.
fn shortest_route(
    db: &HashMap<u32, Lsa>,
    self_id: u32,
    params: &DvDtnParams,
) -> Option<RouteBelief> {
    let self_rec = db.get(&self_id)?;

    // A balloon that hears a tower itself needs no search.
    if let Some(tower_id) = self_rec.tower {
        return Some(RouteBelief {
            tower_id,
            hop_count: 1,
            next_hop: None,
            epoch: self_rec.seq,
            emitted_at_round: self_rec.emitted_at_round,
        });
    }

    // (node, depth, first hop out of self, freshness of the stalest record used)
    let mut queue: VecDeque<(u32, u32, u32, u64)> = VecDeque::new();
    let mut seen: HashSet<u32> = HashSet::new();
    seen.insert(self_id);
    for &nb in &self_rec.neighbors {
        if seen.insert(nb) {
            queue.push_back((nb, 1, nb, self_rec.emitted_at_round));
        }
    }

    while let Some((node, depth, first_hop, age)) = queue.pop_front() {
        let Some(rec) = db.get(&node) else {
            continue; // heard of by a neighbour, but no observation of its own
        };
        let age = age.min(rec.emitted_at_round);

        if let Some(tower_id) = rec.tower {
            return Some(RouteBelief {
                tower_id,
                // `depth` hops to reach it, plus its own hop to the ground.
                hop_count: depth + 1,
                next_hop: Some(first_hop),
                epoch: rec.seq,
                emitted_at_round: age,
            });
        }
        if depth >= params.beacon_max_hops {
            continue;
        }
        for &nb in &rec.neighbors {
            if seen.insert(nb) {
                queue.push_back((nb, depth + 1, first_hop, age));
            }
        }
    }
    None
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

    fn linkstate() -> super::super::DvDtn {
        let p = DvDtnParams {
            discovery: Discovery::LinkState,
            ..Default::default()
        };
        let mut d = super::super::DvDtn::with_params(p);
        d.reseed(5);
        d
    }

    /// t0 - b0 - b1 - b2. Nobody is told a distance; b2 has to assemble one
    /// from other balloons' neighbour lists and search it.
    #[test]
    fn a_route_is_computed_from_gossiped_neighbour_lists() {
        let mut d = linkstate();
        let balloons: Vec<Balloon> =
            (0..3).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..3 {
            d.spawn_node();
        }
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2")], 3, &towers);

        for round in 0..200u64 {
            d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
        }

        let b2 = d.nodes[2].belief.expect("b2 should have computed a route");
        assert_eq!(b2.next_hop, Some(1), "the first hop out of b2 is b1");
        assert_eq!(b2.hop_count, 3, "two balloon hops plus the ground hop");
        assert!(d.stats.delivered >= 1, "and bundles should flow: {:?}", d.stats);
    }

    /// The property distance-vector cannot express: when the map stops showing
    /// a path, the belief goes away *because the map says so*, not because a
    /// lease ran out. b0 is the only way to the tower, so severing it leaves
    /// b1 and b2 with a connected mesh and no route in it.
    #[test]
    fn a_node_whose_map_shows_no_path_drops_its_route() {
        let mut d = linkstate();
        let balloons: Vec<Balloon> =
            (0..3).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..3 {
            d.spawn_node();
        }
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2")], 3, &towers);
        for round in 0..200u64 {
            d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
        }
        assert!(d.nodes[2].belief.is_some(), "setup: b2 should have a route");

        // b0 drifts out of range of everything. b1 and b2 still hear each
        // other, so gossip keeps flowing and their maps stay current.
        adj.rebuild(&[edge("b1", "b2")], 3, &towers);
        for round in 200..320u64 {
            d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj });
        }

        assert!(
            d.nodes[2].belief.is_none(),
            "b2's map no longer contains a tower, so it should hold no route: {:?}",
            d.nodes[2].belief
        );
    }

    /// A path is only as current as the stalest observation it rests on.
    /// Without this, a route across four old links would look as fresh as its
    /// newest hop and would never expire.
    #[test]
    fn a_computed_route_carries_the_age_of_its_stalest_link() {
        let mut db = HashMap::new();
        db.insert(
            0,
            Lsa { origin: 0, seq: 1, emitted_at_round: 100, neighbors: vec![1], tower: None },
        );
        // The middle observation is much older than the ones on either side.
        db.insert(
            1,
            Lsa { origin: 1, seq: 1, emitted_at_round: 20, neighbors: vec![0, 2], tower: None },
        );
        db.insert(
            2,
            Lsa { origin: 2, seq: 1, emitted_at_round: 99, neighbors: vec![1], tower: Some(7) },
        );

        let r = shortest_route(&db, 0, &DvDtnParams::default()).expect("a path exists");
        assert_eq!(r.tower_id, 7);
        assert_eq!(r.hop_count, 3);
        assert_eq!(r.next_hop, Some(1));
        assert_eq!(
            r.emitted_at_round, 20,
            "the route rests on a round-20 observation, so that is how old it is"
        );
    }

    /// Gossip carries observations, never conclusions — so a balloon's own
    /// record is what it can hear right now, and nothing in the batch claims a
    /// distance to anything.
    #[test]
    fn gossip_is_the_only_kind_on_the_wire() {
        let mut d = linkstate();
        let balloons: Vec<Balloon> =
            (0..3).map(|i| Balloon::new(i, i as f64, 0.0, 18000.0)).collect();
        for _ in 0..3 {
            d.spawn_node();
        }
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2")], 3, &towers);

        let mut kinds: Vec<EventKind> = Vec::new();
        for round in 0..60u64 {
            for e in d.step(StepCtx { round, balloons: &balloons, towers: &towers, adj: &adj }) {
                if !kinds.contains(&e.kind) {
                    kinds.push(e.kind);
                }
            }
        }
        assert!(kinds.contains(&EventKind::Gossip));
        assert!(
            !kinds.contains(&EventKind::RouteAd) && !kinds.contains(&EventKind::RouteReply),
            "link-state advertises no routes, only observations: {kinds:?}"
        );
    }

    /// Like reactive discovery, this variant sends no tower beacons, so the
    /// beacon-borne ack digest has nothing to ride on.
    #[test]
    fn link_state_refuses_the_beacon_borne_ack_digest() {
        let err = "dv-dtn:discovery=link-state,ack=digest".parse::<ProtocolSpec>().unwrap_err();
        assert!(err.contains("tower beacons"), "unhelpful error: {err}");
        assert!("dv-dtn:discovery=link-state".parse::<ProtocolSpec>().is_ok());
    }
}
