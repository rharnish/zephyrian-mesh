// Telemetry bundles — C2 of MESH_COMMS_DESIGN.md.
//
// A bundle is data a balloon wants on the ground. It gets there by being handed
// balloon to balloon along each holder's *believed* next hop — a local, possibly
// stale, possibly suboptimal choice. Nothing here consults the edge graph or the
// union-find on a balloon's behalf; a balloon can only forward to a neighbour it
// currently believes leads somewhere, and the transmission only lands if that
// neighbour is genuinely in range right now.
//
// Two rules give this its character, both from §4 of the design doc:
//
//   * **One hop per duty-cycle slot.** A bundle moves only when its holder wakes
//     to transmit, on the same slot it beacons on. A hop therefore costs
//     BEACON_INTERVAL_ROUNDS, and a d-hop path costs d times that. Near the
//     percolation threshold that exceeds a belief's lifetime, so deep bundles
//     strand — which is the designed behaviour, not a failure.
//   * **A stale next hop holds, it does not drop.** If the believed next hop is
//     no longer in range the bundle waits for the belief to refresh. That is what
//     makes this delay-tolerant rather than merely lossy.
//
// Not here yet (slice 2): tower acks source-routed back along `path`, and
// satellite fallback on ack timeout. Until then a bundle that reaches a tower is
// simply delivered, and `BUNDLE_MAX_AGE_ROUNDS` expiry stands in for the timeout
// that will later hand it to satellite.

use crate::balloon::Balloon;
use crate::beacon::MeshAdjacency;
use crate::config::*;

/// One unit of telemetry in transit. The payload is deliberately absent — the
/// environmental sensor block needs `atmosphere.rs` (P1), and nothing about
/// routing depends on it.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub origin_id: u32,
    pub seq: u64,
    pub created_at_round: u64,
    /// Every balloon that has held this bundle, in order, origin first. The
    /// current holder is always `path.last()`. This single field provides loop
    /// detection, the hop budget, and the provenance that C3's relay
    /// attestations will sign.
    pub path: Vec<u32>,
}

impl Bundle {
    pub fn hops(&self) -> usize {
        self.path.len().saturating_sub(1)
    }

    pub fn age(&self, round: u64) -> u64 {
        round.saturating_sub(self.created_at_round)
    }
}

/// Cumulative outcomes. Every bundle that leaves circulation lands in exactly
/// one of the `delivered` / `dropped_*` / `expired` counters — the harness
/// asserts that, since a bundle that quietly vanishes would be invisible in the
/// UI and fatal to the delivery statistics.
#[derive(Debug, Default, Clone, Copy)]
pub struct BundleStats {
    pub originated: u64,
    pub delivered: u64,
    pub dropped_loop: u64,
    pub dropped_ttl: u64,
    pub expired: u64,
    /// Handoffs that failed because the receiver was already carrying. Not a
    /// loss — the sender keeps the bundle and retries on its next slot — so this
    /// is a congestion *pressure* gauge, not an outcome, and is excluded from
    /// `resolved()`.
    pub blocked: u64,
}

impl BundleStats {
    /// Bundles that have left circulation, however they left.
    pub fn resolved(&self) -> u64 {
        self.delivered + self.dropped_loop + self.dropped_ttl + self.expired
    }
}

/// Advance bundles by one comms round.
///
/// `awake` is the set of balloon indices that transmitted this round, taken
/// straight from `beacon::step` — sharing that set is what ties forwarding to
/// the radio duty cycle rather than giving bundles a schedule of their own.
pub fn step(
    balloons: &mut [Balloon],
    adj: &MeshAdjacency,
    awake: &[usize],
    round: u64,
    stats: &mut BundleStats,
) {
    // 1. Expire. A bundle nobody could move for this long is out of options;
    //    slice 2 will hand these to satellite instead of dropping them.
    for b in balloons.iter_mut() {
        if b.carrying.as_ref().is_some_and(|bd| bd.age(round) > BUNDLE_MAX_AGE_ROUNDS) {
            b.carrying = None;
            stats.expired += 1;
        }
    }

    // 2. Gather this round's transmissions. Two-phase for the same reason as
    //    beacon::step — applying in place would let a bundle race along several
    //    hops within one round depending on iteration order, collapsing exactly
    //    the delay being modelled.
    let mut moves: Vec<(usize, u32)> = Vec::new(); // (from index, to balloon id)

    for &i in awake {
        let Some(bundle) = balloons[i].carrying.as_ref() else { continue };

        // A balloon with no belief has nowhere to send it. Hold.
        let Some(belief) = balloons[i].belief else { continue };

        match belief.next_hop {
            // Believes it hears a tower directly. Only true if the link is
            // still live — belief can be stale, the radio cannot lie.
            None => {
                if adj.tower_in_range(i).is_some() {
                    balloons[i].carrying = None;
                    stats.delivered += 1;
                }
                // Otherwise hold: the belief will expire or refresh.
            }
            Some(next) => {
                if !adj.is_neighbor(i, next) {
                    continue; // stale next hop — hold, don't drop
                }
                if bundle.path.contains(&next) {
                    balloons[i].carrying = None;
                    stats.dropped_loop += 1;
                    continue;
                }
                if bundle.path.len() >= BUNDLE_MAX_HOPS {
                    balloons[i].carrying = None;
                    stats.dropped_ttl += 1;
                    continue;
                }
                moves.push((i, next));
            }
        }
    }

    // 3. Apply. A receiver already holding a bundle has nowhere to put it, so
    //    the handoff simply fails and the sender keeps carrying — the same
    //    hold-don't-drop rule as a stale next hop. Dropping here instead loses
    //    the overwhelming majority of traffic the moment the mesh is busy, which
    //    is what the first run of bundle_delivery.rs showed: 18270 of 21634
    //    bundles destroyed by congestion alone.
    for (from, to) in moves {
        let to_idx = to as usize;
        let free = balloons.get(to_idx).is_some_and(|r| r.carrying.is_none());
        if !free {
            stats.blocked += 1;
            continue;
        }
        let Some(mut bundle) = balloons[from].carrying.take() else { continue };
        bundle.path.push(to);
        balloons[to_idx].carrying = Some(bundle);
    }

    // 4. Originate. Bounded by the single carry slot: a balloon still holding
    //    something produces nothing new.
    for &i in awake {
        let b = &mut balloons[i];
        if b.carrying.is_some() || round < b.next_bundle_round {
            continue;
        }
        b.carrying = Some(Bundle {
            origin_id: b.id,
            seq: b.bundle_seq,
            created_at_round: round,
            path: vec![b.id],
        });
        b.bundle_seq += 1;
        b.next_bundle_round = round + BUNDLE_INTERVAL_ROUNDS;
        stats.originated += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beacon::RouteBelief;
    use crate::link_detection::Edge;
    use crate::tower::Tower;

    fn edge(a: &str, b: &str) -> Edge {
        Edge { a_key: a.to_string(), b_key: b.to_string() }
    }

    fn line(n: usize) -> (Vec<Balloon>, Vec<Tower>, MeshAdjacency) {
        // t0 -- b0 -- b1 -- ... -- b(n-1)
        let balloons: Vec<Balloon> =
            (0..n).map(|i| Balloon::new(i as u32, i as f64, 0.0, 18000.0)).collect();
        let towers = vec![Tower::new(0, -1.0, 0.0, 30.0)];
        let mut edges = vec![edge("t0", "b0")];
        for i in 0..n.saturating_sub(1) {
            edges.push(edge(&format!("b{i}"), &format!("b{}", i + 1)));
        }
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&edges, n, &towers);
        (balloons, towers, adj)
    }

    /// Give every balloon a correct belief pointing one hop closer to the tower.
    fn seed_beliefs(balloons: &mut [Balloon], round: u64) {
        for i in 0..balloons.len() {
            balloons[i].belief = Some(RouteBelief {
                tower_id: 0,
                hop_count: i as u32 + 1,
                next_hop: if i == 0 { None } else { Some(i as u32 - 1) },
                epoch: 1,
                emitted_at_round: round,
            });
        }
    }

    #[test]
    fn a_bundle_walks_the_chain_and_is_delivered() {
        let (mut balloons, _t, adj) = line(4);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        let awake: Vec<usize> = (0..4).collect();

        // Only b3 originates; the rest just relay.
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[3].next_bundle_round = 0;

        for round in 0..10 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
            if stats.delivered > 0 {
                break;
            }
        }
        assert_eq!(stats.delivered, 1, "stats: {stats:?}");
        assert_eq!(stats.dropped_loop + stats.dropped_ttl + stats.expired, 0);
    }

    #[test]
    fn path_history_records_every_relay_in_order() {
        let (mut balloons, _t, adj) = line(4);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        let awake: Vec<usize> = (0..4).collect();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[3].next_bundle_round = 0;

        step(&mut balloons, &adj, &awake, 0, &mut stats); // b3 originates
        assert_eq!(balloons[3].carrying.as_ref().unwrap().path, vec![3]);

        step(&mut balloons, &adj, &awake, 1, &mut stats); // 3 -> 2
        assert_eq!(balloons[2].carrying.as_ref().unwrap().path, vec![3, 2]);

        step(&mut balloons, &adj, &awake, 2, &mut stats); // 2 -> 1
        let held = balloons[1].carrying.as_ref().unwrap();
        assert_eq!(held.path, vec![3, 2, 1]);
        assert_eq!(held.hops(), 2);
    }

    /// The delay-tolerance rule: a next hop that has gone out of range makes the
    /// bundle *wait*, not vanish.
    #[test]
    fn a_stale_next_hop_holds_the_bundle_rather_than_dropping_it() {
        let (mut balloons, towers, _adj) = line(3);
        // Rebuild adjacency with the b1--b0 link missing, so b1's belief that it
        // can reach b0 is stale.
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("t0", "b0"), edge("b1", "b2")], 3, &towers);
        seed_beliefs(&mut balloons, 0);

        let mut stats = BundleStats::default();
        let awake: Vec<usize> = (0..3).collect();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[1].carrying = Some(Bundle {
            origin_id: 1,
            seq: 0,
            created_at_round: 0,
            path: vec![1],
        });

        for round in 0..10 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
        }
        assert!(balloons[1].carrying.is_some(), "should still be holding");
        assert_eq!(stats.resolved(), 0, "nothing should have resolved: {stats:?}");
    }

    #[test]
    fn a_bundle_offered_back_to_a_balloon_already_in_its_path_is_dropped_as_a_loop() {
        let (mut balloons, towers, _a) = line(2);
        let mut adj = MeshAdjacency::default();
        adj.rebuild(&[edge("b0", "b1")], 2, &towers);

        // b1 believes b0 is its next hop; b0 believes b1 is. A bundle handed
        // between them must not ping-pong.
        balloons[0].belief = Some(RouteBelief {
            tower_id: 0, hop_count: 3, next_hop: Some(1), epoch: 1, emitted_at_round: 0,
        });
        balloons[1].belief = Some(RouteBelief {
            tower_id: 0, hop_count: 3, next_hop: Some(0), epoch: 1, emitted_at_round: 0,
        });
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[1].carrying = Some(Bundle {
            origin_id: 1, seq: 0, created_at_round: 0, path: vec![1],
        });

        let mut stats = BundleStats::default();
        let awake: Vec<usize> = vec![0, 1];
        for round in 0..6 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
        }
        assert_eq!(stats.dropped_loop, 1, "stats: {stats:?}");
        assert!(balloons.iter().all(|b| b.carrying.is_none()));
    }

    #[test]
    fn a_bundle_nobody_can_move_eventually_expires() {
        // One balloon, no links, no belief: it originates and can never send.
        let mut balloons = vec![Balloon::new(0, 0.0, 0.0, 18000.0)];
        let adj = MeshAdjacency::default();
        let mut stats = BundleStats::default();

        for round in 0..(BUNDLE_MAX_AGE_ROUNDS + 5) {
            step(&mut balloons, &adj, &[0], round, &mut stats);
        }
        assert!(stats.expired >= 1, "stats: {stats:?}");
        assert_eq!(stats.delivered, 0);
    }
}
