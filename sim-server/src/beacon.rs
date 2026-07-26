// Decentralized connectivity discovery — C1 of MESH_COMMS_DESIGN.md.
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
// BELIEF_MAX_AGE_ROUNDS after the link actually broke, and can believe it is
// isolated while sitting on a perfectly good path nobody has told it about.
// Neither is a bug to fix.

use crate::balloon::Balloon;
use crate::config::*;
use crate::link_detection::Edge;
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
    pub fn is_expired(&self, round: u64) -> bool {
        round.saturating_sub(self.emitted_at_round) > BELIEF_MAX_AGE_ROUNDS
    }

    pub fn age(&self, round: u64) -> u64 {
        round.saturating_sub(self.emitted_at_round)
    }
}

/// Who can hear whom. Rebuilt from the edge list whenever links are
/// recomputed. Balloon slots are indexed by position in the visible slice
/// (== balloon id, since ids are assigned sequentially and never reused);
/// tower slots by position in the tower vec, since tower ids *can* be removed.
#[derive(Default)]
pub struct MeshAdjacency {
    balloon_adj: Vec<Vec<u32>>,
    tower_adj: Vec<Vec<u32>>,
}

fn parse_key(key: &str) -> Option<(bool, u32)> {
    let (tag, rest) = key.split_at(1);
    let id: u32 = rest.parse().ok()?;
    match tag {
        "b" => Some((true, id)),
        "t" => Some((false, id)),
        _ => None,
    }
}

impl MeshAdjacency {
    pub fn rebuild(&mut self, edges: &[Edge], n_balloons: usize, towers: &[Tower]) {
        self.balloon_adj.clear();
        self.balloon_adj.resize(n_balloons, Vec::new());
        self.tower_adj.clear();
        self.tower_adj.resize(towers.len(), Vec::new());

        let tower_slot: HashMap<u32, usize> =
            towers.iter().enumerate().map(|(i, t)| (t.id, i)).collect();

        for e in edges {
            let (Some(a), Some(b)) = (parse_key(&e.a_key), parse_key(&e.b_key)) else {
                continue;
            };
            match (a, b) {
                ((true, x), (true, y)) => {
                    // Balloon-to-balloon: symmetric, both directions.
                    if let Some(v) = self.balloon_adj.get_mut(x as usize) {
                        v.push(y);
                    }
                    if let Some(v) = self.balloon_adj.get_mut(y as usize) {
                        v.push(x);
                    }
                }
                // Tower-to-balloon is only ever used in the tower->balloon
                // direction: towers originate beacons, they don't relay them.
                ((true, b_id), (false, t_id)) | ((false, t_id), (true, b_id)) => {
                    if let Some(&slot) = tower_slot.get(&t_id) {
                        self.tower_adj[slot].push(b_id);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Should `new` replace `cur`? This is the rebroadcast-suppression rule: a
/// beacon that doesn't improve the belief is dropped rather than relayed,
/// which is what stops a flood from becoming a broadcast storm.
fn should_adopt(cur: Option<&RouteBelief>, new: &RouteBelief) -> bool {
    let Some(cur) = cur else { return true };
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

fn next_slot(round: u64, rng: &mut impl Rng) -> u64 {
    let jitter = rng.gen_range(0..=(2 * BEACON_JITTER_ROUNDS)) as i64 - BEACON_JITTER_ROUNDS as i64;
    let interval = (BEACON_INTERVAL_ROUNDS as i64 + jitter).max(1) as u64;
    round + interval
}

/// Randomized initial beacon phase, so the whole fleet doesn't transmit on the
/// same round. Called at spawn.
pub fn initial_slot(rng: &mut impl Rng) -> u64 {
    rng.gen_range(0..BEACON_INTERVAL_ROUNDS)
}

/// Advance discovery by one round. `balloons` must be the visible slice — only
/// visible balloons participate in link detection, so only they can hear or
/// be heard. (A balloon hidden by the slider keeps its belief until it simply
/// ages out, and rediscovers from scratch if it becomes visible again.)
pub fn step(
    balloons: &mut [Balloon],
    towers: &mut [Tower],
    adj: &MeshAdjacency,
    round: u64,
    rng: &mut impl Rng,
) {
    // 1. Expire first, so nothing rebroadcasts a belief it should have dropped.
    for b in balloons.iter_mut() {
        if b.belief.is_some_and(|bel| bel.is_expired(round)) {
            b.belief = None;
        }
    }

    // 2. Gather this round's transmissions. Two-phase because a transmission
    //    reads one node's belief and writes its neighbours' — and because
    //    applying immediately would let a beacon race across many hops within
    //    a single round depending on iteration order, collapsing exactly the
    //    propagation delay we're modelling.
    let mut offers: Vec<(usize, RouteBelief)> = Vec::new();

    for (slot, t) in towers.iter_mut().enumerate() {
        if round < t.next_beacon_round {
            continue;
        }
        t.next_beacon_round = next_slot(round, rng);
        t.beacon_epoch += 1;
        for &b_id in adj.tower_adj.get(slot).into_iter().flatten() {
            offers.push((
                b_id as usize,
                RouteBelief {
                    tower_id: t.id,
                    hop_count: 1,
                    next_hop: None,
                    epoch: t.beacon_epoch,
                    emitted_at_round: round,
                },
            ));
        }
    }

    for i in 0..balloons.len() {
        if round < balloons[i].next_beacon_round {
            continue;
        }
        balloons[i].next_beacon_round = next_slot(round, rng);
        // A balloon with no belief has nothing to say. Silence is itself
        // information the neighbours never get — they can't tell "no route"
        // from "not transmitting".
        let Some(belief) = balloons[i].belief else { continue };
        if belief.hop_count >= BEACON_MAX_HOPS {
            continue;
        }
        let id = balloons[i].id;
        for &n_id in adj.balloon_adj.get(i).into_iter().flatten() {
            offers.push((
                n_id as usize,
                // `emitted_at_round` is inherited untouched via `..belief` —
                // relaying does not make the news any newer.
                RouteBelief {
                    hop_count: belief.hop_count + 1,
                    next_hop: Some(id),
                    ..belief
                },
            ));
        }
    }

    // 3. Apply.
    for (idx, offer) in offers {
        let Some(b) = balloons.get_mut(idx) else { continue };
        if offer.next_hop == Some(b.id) {
            continue; // never learn a route to the ground from yourself
        }
        if offer.is_expired(round) {
            continue; // news too old to act on, whoever just repeated it
        }
        if should_adopt(b.belief.as_ref(), &offer) {
            b.belief = Some(offer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_detection::Edge;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn edge(a: &str, b: &str) -> Edge {
        Edge { a_key: a.to_string(), b_key: b.to_string() }
    }

    /// A chain t0 - b0 - b1 - b2 should light up one hop at a time, not all at
    /// once: the propagation delay is the whole point of the design.
    #[test]
    fn belief_propagates_outward_one_hop_at_a_time() {
        let mut balloons: Vec<Balloon> =
            (0..3).map(|i| Balloon::new(i, 0.0, 0.0, 10000.0)).collect();
        let mut towers = vec![Tower::new(0, 0.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b0", "b1"), edge("b1", "b2")], 3, &towers);
        let mut rng = StdRng::seed_from_u64(7);

        let mut first_seen = [None; 3];
        for round in 0..60u64 {
            step(&mut balloons, &mut towers, &adj, round, &mut rng);
            for (i, b) in balloons.iter().enumerate() {
                if first_seen[i].is_none() && b.belief.is_some() {
                    first_seen[i] = Some(round);
                }
            }
        }

        let t = first_seen.map(|x| x.expect("every balloon in the chain should learn a route"));
        assert!(t[0] < t[1] && t[1] < t[2], "beacon should reach nearer balloons first, got {t:?}");
        assert_eq!(balloons[0].belief.unwrap().hop_count, 1);
        assert_eq!(balloons[1].belief.unwrap().hop_count, 2);
        assert_eq!(balloons[2].belief.unwrap().hop_count, 3);
        // Learned next-hop must point back the way the beacon came.
        assert_eq!(balloons[0].belief.unwrap().next_hop, None);
        assert_eq!(balloons[1].belief.unwrap().next_hop, Some(0));
        assert_eq!(balloons[2].belief.unwrap().next_hop, Some(1));
    }

    /// Cut the link and the belief must not persist forever — it ages out,
    /// and only then does the balloon consider itself ungrounded.
    #[test]
    fn belief_expires_after_link_is_cut() {
        let mut balloons: Vec<Balloon> = vec![Balloon::new(0, 0.0, 0.0, 10000.0)];
        let mut towers = vec![Tower::new(0, 0.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0")], 1, &towers);
        let mut rng = StdRng::seed_from_u64(11);

        let mut round = 0u64;
        while round < 30 && balloons[0].belief.is_none() {
            step(&mut balloons, &mut towers, &adj, round, &mut rng);
            round += 1;
        }
        assert!(balloons[0].belief.is_some(), "should have learned a route");

        // Sever every link, keep ticking.
        adj.rebuild(&[], 1, &towers);
        let cut_at = round;
        while balloons[0].belief.is_some() {
            step(&mut balloons, &mut towers, &adj, round, &mut rng);
            round += 1;
            assert!(round - cut_at < 100, "belief should have expired by now");
        }
        // It must outlive the cut by roughly the timeout, not vanish instantly —
        // that lag is the belief/truth divergence we want to visualize.
        assert!(round - cut_at > BELIEF_MAX_AGE_ROUNDS / 4);
    }

    /// An isolated balloon must never invent a route. "No signal" and "no
    /// route" have to look the same from inside.
    #[test]
    fn isolated_balloon_never_believes_it_is_grounded() {
        let mut balloons: Vec<Balloon> = vec![Balloon::new(0, 0.0, 0.0, 10000.0)];
        let mut towers = vec![Tower::new(0, 0.0, 0.0, 30.0)];
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[], 1, &towers);
        let mut rng = StdRng::seed_from_u64(3);
        for round in 0..200 {
            step(&mut balloons, &mut towers, &adj, round, &mut rng);
            assert!(balloons[0].belief.is_none());
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
        assert!(should_adopt(Some(&long), &short));
        assert!(!should_adopt(Some(&short), &long));
        // A fresher wave supersedes regardless of hop count.
        let fresher = RouteBelief { epoch: 5, emitted_at_round: 105, hop_count: 8, ..long };
        assert!(should_adopt(Some(&short), &fresher));
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
        assert!(!should_adopt(Some(&held), &echoed));
    }

    /// Cut every tower loose and the whole field must forget, not settle into
    /// a plateau of self-sustaining stale beliefs.
    #[test]
    fn beliefs_drain_completely_once_towers_are_unreachable() {
        let n = 12usize;
        let mut balloons: Vec<Balloon> =
            (0..n as u32).map(|i| Balloon::new(i, 0.0, 0.0, 10000.0)).collect();
        let mut towers = vec![Tower::new(0, 0.0, 0.0, 30.0)];
        // Tower feeds b0; the rest form a densely interconnected clump, which
        // is the structure that lets stale routes circulate.
        let mut edges = vec![edge("t0", "b0")];
        for i in 0..n {
            for j in (i + 1)..n {
                edges.push(edge(&format!("b{i}"), &format!("b{j}")));
            }
        }
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&edges, n, &towers);
        let mut rng = StdRng::seed_from_u64(5);

        let mut round = 0u64;
        while round < 120 {
            step(&mut balloons, &mut towers, &adj, round, &mut rng);
            round += 1;
        }
        assert!(
            balloons.iter().all(|b| b.belief.is_some()),
            "everything should be reachable while the tower is connected"
        );

        // Sever only the tower link; the balloon-to-balloon clump stays intact.
        adj.rebuild(&edges[1..], n, &towers);
        let cut_at = round;
        while round < cut_at + 300 {
            step(&mut balloons, &mut towers, &adj, round, &mut rng);
            round += 1;
        }
        let survivors = balloons.iter().filter(|b| b.belief.is_some()).count();
        assert_eq!(survivors, 0, "{survivors} balloons kept a dead route alive");
    }
}
