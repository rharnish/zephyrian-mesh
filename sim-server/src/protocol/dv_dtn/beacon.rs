// Decentralized connectivity discovery — C1 of docs/design/MESH_COMMS_DESIGN.md.
//
// The rule this module exists to enforce: **a balloon is never told whether it
// can reach a tower.** It has no access to the edge list, the union-find, or
// the `grounded` flag sim.rs computes. Everything it believes about the
// network came from a beacon that physically arrived at it.
//
// Mechanism is periodic distance-vector. Towers flood hop-counted beacons;
// a balloon that hears one records a `RouteBelief` and rebroadcasts it one hop
// further on its *own* duty-cycle slot (not immediately on receipt) — which is
// what rate-limits the flood and makes propagation take roughly one beacon
// interval per hop. Beliefs expire if no fresher beacon refreshes them.
//
// Consequence, and the point of the design: belief and ground truth drift
// apart in both directions. A balloon keeps believing in a route for up to
// belief_max_age_rounds after the link actually broke, and can believe it is
// isolated while sitting on a perfectly good path nobody has told it about.
// Neither is a bug to fix.

use crate::balloon::Balloon;
use super::params::{AckPolicy, DvDtnParams, Metric};
use super::{DvNode, TowerBeacon};
use crate::link_detection::NodeKey;
use crate::mesh_adjacency::MeshAdjacency;
use crate::tower::Tower;
use rand::Rng;
use std::collections::HashMap;

/// What one balloon currently thinks it knows about reaching the ground.
/// Four fields — this is the entire per-balloon routing state.
#[derive(Debug, Clone, Copy)]
pub struct RouteBelief {
    pub tower_id: u32,
    pub hop_count: u32,
    /// Balloon this was heard from — the next hop toward the tower. `None`
    /// means it was heard from the tower directly (hop_count == 1).
    pub next_hop: Option<u32>,
    /// Which beacon wave this came from. Monotonic per tower, so a later wave
    /// always supersedes an earlier one from the same tower.
    pub epoch: u64,
    /// Tick the *tower* emitted this wave. Relays copy it verbatim and may
    /// never restamp it — this is the belief's true age, and the only thing
    /// expiry is allowed to key on.
    pub emitted_at_round: u64,
}

impl RouteBelief {
    /// Expiry is by age of the news, deliberately *not* by how recently some
    /// neighbour last repeated it. The latter is self-sustaining: balloons cut
    /// off from every tower keep renewing each other's dead routes by passing
    /// them in circles, and the field never forgets.
    pub fn is_expired(&self, round: u64, max_age: u64) -> bool {
        round.saturating_sub(self.emitted_at_round) > max_age
    }

    pub fn age(&self, round: u64) -> u64 {
        round.saturating_sub(self.emitted_at_round)
    }
}

/// Should `new` replace `cur`? This is the rebroadcast-suppression rule: a
/// beacon that doesn't improve the belief is dropped rather than relayed,
/// which is what stops a flood from becoming a broadcast storm.
fn should_adopt(cur: Option<&RouteBelief>, new: &RouteBelief, params: &DvDtnParams) -> bool {
    let Some(cur) = cur else { return true };
    if params.metric == Metric::NearestFirst {
        // Nearest wins, freshness only breaks ties. Beliefs still drain when
        // towers go away, because that property comes from expiry (which is
        // keyed on emitted_at_round) rather than from this comparison.
        return new.hop_count < cur.hop_count
            || (new.hop_count == cur.hop_count && new.emitted_at_round > cur.emitted_at_round);
    }
    // Freshness is judged by when the tower emitted the wave, which is a global
    // sim round and therefore comparable across towers. Crucially this is the
    // *only* thing that can refresh a belief's lease: re-hearing news you
    // already have does not extend its life, so once the towers go out of
    // earshot every belief downstream ages out instead of circulating forever.
    if new.emitted_at_round != cur.emitted_at_round {
        return new.emitted_at_round > cur.emitted_at_round;
    }
    // Same wave: prefer the shorter path it arrived by.
    new.hop_count < cur.hop_count
}

fn next_slot(round: u64, params: &DvDtnParams, rng: &mut (impl Rng + ?Sized)) -> u64 {
    let j = params.beacon_jitter_rounds;
    let jitter = rng.gen_range(0..=(2 * j)) as i64 - j as i64;
    let interval = (params.beacon_interval_rounds as i64 + jitter).max(1) as u64;
    round + interval
}

/// Randomized initial beacon phase, so the whole fleet doesn't transmit on the
/// same round. Called at spawn.
pub fn initial_slot(params: &DvDtnParams, rng: &mut (impl Rng + ?Sized)) -> u64 {
    rng.gen_range(0..params.beacon_interval_rounds)
}

/// One beacon transmission from this round, for the wavefront animation
/// (docs/design/MESH_COMMS_DESIGN.md §3 "Beacon wavefront"). `from` is the
/// tower for a fresh wave's first hop, or the relaying balloon otherwise —
/// exactly the node whose radio actually carried this offer.
pub struct BeaconHop {
    pub from: NodeKey,
    pub to_balloon: usize,
    pub tower_id: u32,
    pub hop_count: u32,
    pub epoch: u64,
}

/// An origin learning, from a digest that reached it, that its bundle landed.
/// Returned rather than applied in place because resolving it also has to
/// touch stats and the retained telemetry record, which live with bundles.
pub struct DigestAck {
    pub node: usize,
    pub seq: u64,
}

pub struct BeaconStepResult {
    /// Indices of balloons that woke and transmitted this round. Bundle
    /// forwarding reuses this set rather than keeping its own schedule: a
    /// radio that is awake is awake for both, which is what makes
    /// beacon_interval_rounds the forwarding rate as well (see
    /// docs/design/MESH_COMMS_DESIGN.md §4).
    pub awake: Vec<usize>,
    /// Every offer made this round, for whichever tower(s) a client wants to
    /// animate. Not filtered here — see Snapshot::comms_events.
    pub hops: Vec<BeaconHop>,
    /// Origins that heard their own delivery announced (AckPolicy::Digest).
    pub digest_acks: Vec<DigestAck>,
}

/// Advance discovery by one round. `nodes` and `balloons` must be the visible
/// slices, index-aligned — only visible balloons participate in link
/// detection, so only they can hear or be heard. (A balloon hidden by the
/// slider keeps its belief until it simply ages out, and rediscovers from
/// scratch if it becomes visible again.)
///
/// `balloons` is read-only: discovery reads identity, never physics state.
pub fn step(
    nodes: &mut [DvNode],
    balloons: &[Balloon],
    tower_state: &mut HashMap<u32, TowerBeacon>,
    towers: &[Tower],
    adj: &MeshAdjacency,
    params: &DvDtnParams,
    round: u64,
    rng: &mut (impl Rng + ?Sized),
) -> BeaconStepResult {
    // 1. Expire first, so nothing rebroadcasts a belief it should have dropped.
    for n in nodes.iter_mut() {
        if n.belief.is_some_and(|bel| bel.is_expired(round, params.belief_max_age_rounds)) {
            n.belief = None;
        }
    }

    // 2. Gather this round's transmissions. Two-phase because a transmission
    //    reads one node's belief and writes its neighbours' — and because
    //    applying immediately would let a beacon race across many hops within
    //    a single round depending on iteration order, collapsing exactly the
    //    propagation delay we're modelling.
    // (target, belief, index into `digests`). The digest is shared by every
    // offer one transmitter makes this round, so it is stored once and
    // referenced — a beacon is a broadcast, and cloning it per neighbour would
    // be both wasteful and a poor model of one.
    let mut offers: Vec<(usize, RouteBelief, usize)> = Vec::new();
    let mut digests: Vec<Vec<(u32, u64)>> = Vec::new();
    let mut hops: Vec<BeaconHop> = Vec::new();
    let digest_on = params.ack_policy == AckPolicy::Digest;

    for (slot, t) in towers.iter().enumerate() {
        let ts = tower_state.entry(t.id).or_default();
        if round < ts.next_round {
            continue;
        }
        ts.next_round = next_slot(round, params, rng);
        ts.epoch += 1;
        let epoch = ts.epoch;
        // Announce the newest deliveries this tower has taken. Older than a
        // belief lifetime and the origin has already given up, so saying so
        // tells it nothing it can act on.
        let d_idx = digests.len();
        digests.push(if digest_on {
            ts.recent
                .iter()
                .rev()
                .filter(|(_, _, at)| round.saturating_sub(*at) <= params.belief_max_age_rounds)
                .take(params.ack_digest_entries)
                .map(|(o, s, _)| (*o, *s))
                .collect()
        } else {
            Vec::new()
        });
        for &b_id in adj.tower_neighbors(slot) {
            let belief = RouteBelief {
                tower_id: t.id,
                hop_count: 1,
                next_hop: None,
                epoch,
                emitted_at_round: round,
            };
            hops.push(BeaconHop {
                from: NodeKey::Tower(t.id),
                to_balloon: b_id as usize,
                tower_id: belief.tower_id,
                hop_count: belief.hop_count,
                epoch: belief.epoch,
            });
            offers.push((b_id as usize, belief, d_idx));
        }
    }

    let mut awake: Vec<usize> = Vec::new();
    for i in 0..nodes.len() {
        if round < nodes[i].next_beacon_round {
            continue;
        }
        nodes[i].next_beacon_round = next_slot(round, params, rng);
        awake.push(i);
        // A balloon with no belief has nothing to say. Silence is itself
        // information the neighbours never get — they can't tell "no route"
        // from "not transmitting".
        let Some(belief) = nodes[i].belief else { continue };
        if belief.hop_count >= params.beacon_max_hops {
            continue;
        }
        let id = balloons[i].id;
        // A relay repeats the announcement it heard, verbatim, exactly as it
        // repeats `emitted_at_round` — that is what makes the digest flood
        // outward for free rather than only reaching the tower's neighbours.
        let d_idx = digests.len();
        digests.push(if digest_on { nodes[i].ack_digest.clone() } else { Vec::new() });
        for &n_id in adj.neighbors(i) {
            // `emitted_at_round` is inherited untouched via `..belief` —
            // relaying does not make the news any newer.
            let offer = RouteBelief {
                hop_count: belief.hop_count + 1,
                next_hop: Some(id),
                ..belief
            };
            hops.push(BeaconHop {
                from: NodeKey::Balloon(id),
                to_balloon: n_id as usize,
                tower_id: offer.tower_id,
                hop_count: offer.hop_count,
                epoch: offer.epoch,
            });
            offers.push((n_id as usize, offer, d_idx));
        }
    }

    // 3. Apply.
    let mut digest_acks: Vec<DigestAck> = Vec::new();
    for (idx, offer, d_idx) in offers {
        let Some(id) = balloons.get(idx).map(|b| b.id) else { continue };
        let Some(n) = nodes.get_mut(idx) else { continue };

        // Hearing a delivery announced is independent of whether the route it
        // arrived on is worth adopting — the balloon received the
        // transmission either way. Checked before the adoption rules for that
        // reason, and skipped entirely for the overwhelming majority of
        // balloons, which have nothing outstanding.
        if digest_on {
            if let Some(out) = n.outstanding.as_ref() {
                if out.state == crate::protocol::dv_dtn::bundle::AckState::Pending
                    && digests[d_idx].iter().any(|&(o, s)| o == id && s == out.seq)
                {
                    digest_acks.push(DigestAck { node: idx, seq: out.seq });
                }
            }
        }

        if offer.next_hop == Some(id) {
            continue; // never learn a route to the ground from yourself
        }
        if offer.is_expired(round, params.belief_max_age_rounds) {
            continue; // news too old to act on, whoever just repeated it
        }
        if should_adopt(n.belief.as_ref(), &offer, params) {
            n.belief = Some(offer);
            if digest_on {
                n.ack_digest = digests[d_idx].clone();
            }
        }
    }

    BeaconStepResult { awake, hops, digest_acks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_detection::{Edge, NodeKey};
use crate::mesh_adjacency::MeshAdjacency;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn node_key(s: &str) -> NodeKey {
        let (tag, rest) = s.split_at(1);
        let id: u32 = rest.parse().unwrap();
        match tag {
            "b" => NodeKey::Balloon(id),
            "t" => NodeKey::Tower(id),
            _ => panic!("bad test node key {s:?}"),
        }
    }

    fn edge(a: &str, b: &str) -> Edge {
        Edge { a: node_key(a), b: node_key(b) }
    }

    /// Balloons plus their protocol state plus tower beacon schedules, which
    /// now live in three places rather than one. Bundling them keeps each test
    /// about discovery rather than about wiring.
    struct Field {
        balloons: Vec<Balloon>,
        nodes: Vec<DvNode>,
        towers: Vec<Tower>,
        tower_state: HashMap<u32, TowerBeacon>,
        params: DvDtnParams,
    }

    impl Field {
        fn new(n: u32, towers: Vec<Tower>) -> Self {
            Field {
                balloons: (0..n).map(|i| Balloon::new(i, 0.0, 0.0, 10000.0)).collect(),
                nodes: vec![DvNode::default(); n as usize],
                tower_state: towers.iter().map(|t| (t.id, TowerBeacon::default())).collect(),
                towers,
                params: DvDtnParams::default(),
            }
        }

        fn step(&mut self, adj: &MeshAdjacency, round: u64, rng: &mut impl Rng) {
            super::step(
                &mut self.nodes,
                &self.balloons,
                &mut self.tower_state,
                &self.towers,
                adj,
                &self.params,
                round,
                rng,
            );
        }

        fn belief(&self, i: usize) -> Option<RouteBelief> {
            self.nodes[i].belief
        }
    }

    /// A chain t0 - b0 - b1 - b2 should light up one hop at a time, not all at
    /// once: the propagation delay is the whole point of the design.
    #[test]
    fn belief_propagates_outward_one_hop_at_a_time() {
        let mut f = Field::new(3, vec![Tower::new(0, 0.0, 0.0, 30.0)]);
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2")], 3, &f.towers);
        let mut rng = StdRng::seed_from_u64(7);

        let mut first_seen = [None; 3];
        for round in 0..60u64 {
            f.step(&adj, round, &mut rng);
            for i in 0..3 {
                if first_seen[i].is_none() && f.belief(i).is_some() {
                    first_seen[i] = Some(round);
                }
            }
        }

        let t = first_seen.map(|x| x.expect("every balloon in the chain should learn a route"));
        assert!(t[0] < t[1] && t[1] < t[2], "beacon should reach nearer balloons first, got {t:?}");
        assert_eq!(f.belief(0).unwrap().hop_count, 1);
        assert_eq!(f.belief(1).unwrap().hop_count, 2);
        assert_eq!(f.belief(2).unwrap().hop_count, 3);
        // Learned next-hop must point back the way the beacon came.
        assert_eq!(f.belief(0).unwrap().next_hop, None);
        assert_eq!(f.belief(1).unwrap().next_hop, Some(0));
        assert_eq!(f.belief(2).unwrap().next_hop, Some(1));
    }

    /// The tower's first hop should surface as a hop from the tower node
    /// itself, not from whatever balloon happens to relay it later.
    #[test]
    fn tower_origin_offer_produces_a_hop_from_the_tower() {
        let mut f = Field::new(1, vec![Tower::new(0, 0.0, 0.0, 30.0)]);
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0")], 1, &f.towers);
        let mut rng = StdRng::seed_from_u64(7);

        let mut round = 0u64;
        loop {
            let result = super::step(
                &mut f.nodes,
                &f.balloons,
                &mut f.tower_state,
                &f.towers,
                &adj,
                &f.params,
                round,
                &mut rng,
            );
            if let Some(hop) = result.hops.iter().find(|h| h.to_balloon == 0) {
                assert_eq!(hop.from, NodeKey::Tower(0));
                assert_eq!(hop.hop_count, 1);
                break;
            }
            round += 1;
            assert!(round < 30, "tower should have beaconed by now");
        }
    }

    /// Cut the link and the belief must not persist forever — it ages out,
    /// and only then does the balloon consider itself ungrounded.
    #[test]
    fn belief_expires_after_link_is_cut() {
        let mut f = Field::new(1, vec![Tower::new(0, 0.0, 0.0, 30.0)]);
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0")], 1, &f.towers);
        let mut rng = StdRng::seed_from_u64(11);

        let mut round = 0u64;
        while round < 30 && f.belief(0).is_none() {
            f.step(&adj, round, &mut rng);
            round += 1;
        }
        assert!(f.belief(0).is_some(), "should have learned a route");

        // Sever every link, keep ticking.
        adj.rebuild(&[], 1, &f.towers);
        let cut_at = round;
        while f.belief(0).is_some() {
            f.step(&adj, round, &mut rng);
            round += 1;
            assert!(round - cut_at < 100, "belief should have expired by now");
        }
        // It must outlive the cut by roughly the timeout, not vanish instantly —
        // that lag is the belief/truth divergence we want to visualize.
        assert!(round - cut_at > f.params.belief_max_age_rounds / 4);
    }

    /// An isolated balloon must never invent a route. "No signal" and "no
    /// route" have to look the same from inside.
    #[test]
    fn isolated_balloon_never_believes_it_is_grounded() {
        let mut f = Field::new(1, vec![Tower::new(0, 0.0, 0.0, 30.0)]);
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[], 1, &f.towers);
        let mut rng = StdRng::seed_from_u64(3);
        for round in 0..200 {
            f.step(&adj, round, &mut rng);
            assert!(f.belief(0).is_none());
        }
    }

    #[test]
    fn shorter_path_within_a_wave_wins() {
        let long = RouteBelief {
            tower_id: 0,
            hop_count: 6,
            next_hop: Some(9),
            epoch: 4,
            emitted_at_round: 100,
        };
        let short = RouteBelief { hop_count: 3, next_hop: Some(2), ..long };
        assert!(should_adopt(Some(&long), &short, &DvDtnParams::default()));
        assert!(!should_adopt(Some(&short), &long, &DvDtnParams::default()));
        // A fresher wave supersedes regardless of hop count.
        let fresher = RouteBelief { epoch: 5, emitted_at_round: 105, hop_count: 8, ..long };
        assert!(should_adopt(Some(&short), &fresher, &DvDtnParams::default()));
    }

    /// The metric is a real rule swap, not a tiebreak tweak: under
    /// NearestFirst a *shorter but older* route beats a fresher long one,
    /// which is exactly backwards from what ships. (Measured: helps below
    /// percolation, hurts above it — see DvDtnParams::metric.)
    #[test]
    fn nearest_first_prefers_a_shorter_path_over_a_fresher_wave() {
        let near_but_old = RouteBelief {
            tower_id: 0,
            hop_count: 2,
            next_hop: Some(1),
            epoch: 4,
            emitted_at_round: 100,
        };
        let far_but_fresh = RouteBelief {
            hop_count: 9,
            next_hop: Some(2),
            epoch: 5,
            emitted_at_round: 130,
            ..near_but_old
        };

        let shipped = DvDtnParams::default();
        let nearest = DvDtnParams { metric: Metric::NearestFirst, ..Default::default() };

        // Freshest-first takes the newer wave however long its path.
        assert!(should_adopt(Some(&near_but_old), &far_but_fresh, &shipped));
        // Nearest-first refuses it, and would take the short one back.
        assert!(!should_adopt(Some(&near_but_old), &far_but_fresh, &nearest));
        assert!(should_adopt(Some(&far_but_fresh), &near_but_old, &nearest));
        // Freshness still breaks ties at equal hop count under nearest-first.
        let same_hops_fresher =
            RouteBelief { hop_count: 2, emitted_at_round: 130, ..near_but_old };
        assert!(should_adopt(Some(&near_but_old), &same_hops_fresher, &nearest));
    }

    /// The laundering guard: re-hearing news you already hold must not renew
    /// its lease. Without this, balloons cut off from every tower keep each
    /// other's dead routes alive indefinitely by passing them in circles.
    #[test]
    fn relaying_does_not_refresh_an_already_known_wave() {
        let held = RouteBelief {
            tower_id: 0,
            hop_count: 3,
            next_hop: Some(1),
            epoch: 4,
            emitted_at_round: 100,
        };
        // A neighbour relays the very same wave back, one hop longer and
        // stamped as heard right now. It must be rejected outright.
        let echoed = RouteBelief { hop_count: 4, next_hop: Some(2), ..held };
        assert!(!should_adopt(Some(&held), &echoed, &DvDtnParams::default()));
    }

    /// Cut every tower loose and the whole field must forget, not settle into
    /// a plateau of self-sustaining stale beliefs.
    #[test]
    fn beliefs_drain_completely_once_towers_are_unreachable() {
        let n = 12usize;
        let mut f = Field::new(n as u32, vec![Tower::new(0, 0.0, 0.0, 30.0)]);
        // Tower feeds b0; the rest form a densely interconnected clump, which
        // is the structure that lets stale routes circulate.
        let mut edges = vec![edge("t0", "b0")];
        for i in 0..n {
            for j in (i + 1)..n {
                edges.push(edge(&format!("b{i}"), &format!("b{j}")));
            }
        }
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&edges, n, &f.towers);
        let mut rng = StdRng::seed_from_u64(5);

        let mut round = 0u64;
        while round < 120 {
            f.step(&adj, round, &mut rng);
            round += 1;
        }
        assert!(
            (0..n).all(|i| f.belief(i).is_some()),
            "everything should be reachable while the tower is connected"
        );

        // Sever only the tower link; the balloon-to-balloon clump stays intact.
        adj.rebuild(&edges[1..], n, &f.towers);
        let cut_at = round;
        while round < cut_at + 300 {
            f.step(&adj, round, &mut rng);
            round += 1;
        }
        let survivors = (0..n).filter(|&i| f.belief(i).is_some()).count();
        assert_eq!(survivors, 0, "{survivors} balloons kept a dead route alive");
    }
}
