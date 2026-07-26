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
//     makes this delay-tolerant rather than merely lossy. The same applies to a
//     receiver whose queue is full.
//
// Each balloon holds a bounded FIFO queue (RELAY_QUEUE_CAPACITY) and transmits
// the head of it once per duty-cycle slot. The queue is separate from the
// one-outstanding rule on origination: capping *carriage* at one meant a balloon
// holding its own bundle could not relay anyone else's, which gridlocked the
// mesh and cost roughly half of all deliveries.
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

    // --- Slot accounting ----------------------------------------------------
    //
    // A bundle's entire budget is BUNDLE_MAX_AGE_ROUNDS / BEACON_INTERVAL_ROUNDS
    // = 30 wake slots. Every slot where the holder wakes holding a bundle and
    // fails to move it burns part of that budget, so the *stall rate* — not the
    // loss counters above — is what decides whether bundles arrive in time.
    // These break that down by cause.
    /// Wake slots where the holder had at least one bundle in hand.
    pub slots_with_bundle: u64,
    /// ...of which the holder had no route belief at all.
    pub stall_no_belief: u64,
    /// ...of which the believed next hop was no longer a neighbour.
    pub stall_stale_next_hop: u64,
    /// ...of which the balloon believed it could hear a tower, but couldn't.
    pub stall_tower_gone: u64,

    // --- Path length --------------------------------------------------------
    //
    // Indexed by hop count, saturating at the last bucket. Compare
    // `belief_hops` against true mesh depth (bin/mesh_depth.rs): if believed
    // distance runs well past topological distance, routing is sending bundles
    // the long way round and the expiry budget is being spent on detours.
    /// Hops actually travelled by bundles that reached a tower.
    pub delivered_hops: [u64; 32],
    /// Hops travelled before running out of time.
    pub expired_hops: [u64; 32],
    /// Believed distance-to-tower, sampled once per wake slot per balloon.
    pub belief_hops: [u64; 32],

    // --- The last hop -------------------------------------------------------
    //
    // Every bundle in the world has to be handed to a tower by a balloon that
    // can currently hear one, and each such balloon can pass exactly one bundle
    // per duty-cycle slot. That makes the tower-adjacent population, not the
    // mesh at large, the throughput limit on delivery.
    /// Summed over rounds: balloons with a tower in radio range.
    pub tower_adjacent_samples: u64,
    /// Rounds over which the above was sampled.
    pub rounds_sampled: u64,
}

fn bump(hist: &mut [u64; 32], n: usize) {
    hist[n.min(31)] += 1;
}

impl BundleStats {
    /// Bundles that have left circulation, however they left.
    pub fn resolved(&self) -> u64 {
        self.delivered + self.dropped_loop + self.dropped_ttl + self.expired
    }

    /// Wake slots that actually moved a bundle (forwarded, delivered, or
    /// dropped). `blocked` is subtracted too: a handoff refused by a full
    /// receiver wastes the slot exactly like a stale next hop does, even though
    /// it costs no bundle.
    pub fn slots_used(&self) -> u64 {
        self.slots_with_bundle.saturating_sub(
            self.stall_no_belief
                + self.stall_stale_next_hop
                + self.stall_tower_gone
                + self.blocked,
        )
    }

    /// Fraction of held-bundle wake slots wasted. This is the number that
    /// decides delivery: at a mean depth of d hops a bundle needs d successful
    /// slots out of the 30 it will ever get.
    pub fn stall_rate(&self) -> f64 {
        if self.slots_with_bundle == 0 {
            return 0.0;
        }
        1.0 - self.slots_used() as f64 / self.slots_with_bundle as f64
    }

    /// Mean number of balloons that could hand a bundle straight to a tower.
    pub fn mean_tower_adjacent(&self) -> f64 {
        if self.rounds_sampled == 0 {
            return 0.0;
        }
        self.tower_adjacent_samples as f64 / self.rounds_sampled as f64
    }

    /// Ceiling on deliveries per round, from the last hop alone: each
    /// tower-adjacent balloon can pass TOWER_CONTACT_BUNDLES per
    /// BEACON_INTERVAL_ROUNDS. Nothing about mesh depth, queue depth, or routing
    /// quality can raise it — only the tower-adjacent population or the size of
    /// a contact.
    pub fn delivery_capacity_per_round(&self) -> f64 {
        self.mean_tower_adjacent() * TOWER_CONTACT_BUNDLES as f64 / BEACON_INTERVAL_ROUNDS as f64
    }

    /// Delivery among bundles that actually finished. `delivered / originated`
    /// counts everything still legitimately in flight at the cutoff as a
    /// failure, which understates delivery badly at these origination rates.
    pub fn completion_rate(&self) -> f64 {
        if self.resolved() == 0 {
            return 0.0;
        }
        self.delivered as f64 / self.resolved() as f64
    }
}

/// Mean of a saturating histogram, ignoring the empty case.
pub fn hist_mean(h: &[u64; 32]) -> f64 {
    let n: u64 = h.iter().sum();
    if n == 0 {
        return 0.0;
    }
    h.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum::<f64>() / n as f64
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
        let before = b.queue.len();
        for bd in b.queue.iter() {
            if bd.age(round) > BUNDLE_MAX_AGE_ROUNDS {
                bump(&mut stats.expired_hops, bd.hops());
            }
        }
        b.queue.retain(|bd| bd.age(round) <= BUNDLE_MAX_AGE_ROUNDS);
        stats.expired += (before - b.queue.len()) as u64;
    }

    // 1b. Sample the last-hop population. This is the delivery bottleneck, so
    //     it gets measured every round rather than inferred from geometry.
    stats.rounds_sampled += 1;
    stats.tower_adjacent_samples +=
        (0..balloons.len()).filter(|&i| adj.tower_in_range(i).is_some()).count() as u64;

    // 2. Gather this round's transmissions. Two-phase for the same reason as
    //    beacon::step — applying in place would let a bundle race along several
    //    hops within one round depending on iteration order, collapsing exactly
    //    the delay being modelled.
    let mut moves: Vec<(usize, u32)> = Vec::new(); // (from index, to balloon id)

    // One transmission per awake radio, so only the head of the queue moves.
    for &i in awake {
        if let Some(bel) = balloons[i].belief {
            bump(&mut stats.belief_hops, bel.hop_count as usize);
        }
        let Some(bundle) = balloons[i].queue.front() else { continue };
        stats.slots_with_bundle += 1;

        // A balloon with no belief has nowhere to send it. Hold.
        let Some(belief) = balloons[i].belief else {
            stats.stall_no_belief += 1;
            continue;
        };

        match belief.next_hop {
            // Believes it hears a tower directly. Only true if the link is
            // still live — belief can be stale, the radio cannot lie.
            None => {
                if adj.tower_in_range(i).is_some() {
                    // A tower contact drains up to TOWER_CONTACT_BUNDLES, not
                    // one. The one-per-slot rule rations *beacon* airtime; a
                    // point-to-point link to a ground station is a different
                    // event, and this is the only lever that acts on the last
                    // hop, which is where the throughput limit actually lives.
                    let n = TOWER_CONTACT_BUNDLES.min(balloons[i].queue.len());
                    for _ in 0..n {
                        let bd = balloons[i].queue.pop_front().expect("checked non-empty");
                        bump(&mut stats.delivered_hops, bd.hops());
                        stats.delivered += 1;
                    }
                } else {
                    // Otherwise hold: the belief will expire or refresh.
                    stats.stall_tower_gone += 1;
                }
            }
            Some(next) => {
                if !adj.is_neighbor(i, next) {
                    stats.stall_stale_next_hop += 1;
                    continue; // stale next hop — hold, don't drop
                }
                if bundle.path.contains(&next) {
                    balloons[i].queue.pop_front();
                    stats.dropped_loop += 1;
                    continue;
                }
                if bundle.path.len() >= BUNDLE_MAX_HOPS {
                    balloons[i].queue.pop_front();
                    stats.dropped_ttl += 1;
                    continue;
                }
                moves.push((i, next));
            }
        }
    }

    // 3. Apply. A receiver whose queue is full has nowhere to put it, so the
    //    handoff fails and the sender keeps carrying — the same hold-don't-drop
    //    rule as a stale next hop. Dropping here instead loses the overwhelming
    //    majority of traffic the moment the mesh is busy, which is what the
    //    first run of bundle_delivery.rs showed: 18270 of 21634 bundles
    //    destroyed by congestion alone.
    for (from, to) in moves {
        let to_idx = to as usize;
        let has_room =
            balloons.get(to_idx).is_some_and(|r| r.queue.len() < RELAY_QUEUE_CAPACITY);
        if !has_room {
            stats.blocked += 1;
            continue;
        }
        let Some(mut bundle) = balloons[from].queue.pop_front() else { continue };
        bundle.path.push(to);
        balloons[to_idx].queue.push_back(bundle);
    }

    // 4. Originate. Two independent limits: the queue must have room (shared
    //    with transit traffic), and the balloon may have only *one of its own*
    //    bundles outstanding — the rule the relay queue was wrongly enforcing.
    for &i in awake {
        let b = &mut balloons[i];
        if round < b.next_bundle_round || b.queue.len() >= RELAY_QUEUE_CAPACITY {
            continue;
        }
        if b.queue.iter().any(|bd| bd.origin_id == b.id) {
            continue; // own bundle still in hand
        }
        b.queue.push_back(Bundle {
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
        assert_eq!(balloons[3].queue.front().unwrap().path, vec![3]);

        step(&mut balloons, &adj, &awake, 1, &mut stats); // 3 -> 2
        assert_eq!(balloons[2].queue.front().unwrap().path, vec![3, 2]);

        step(&mut balloons, &adj, &awake, 2, &mut stats); // 2 -> 1
        let held = balloons[1].queue.front().unwrap();
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
        balloons[1].queue.push_back(Bundle {
            origin_id: 1,
            seq: 0,
            created_at_round: 0,
            path: vec![1],
        });

        for round in 0..10 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
        }
        assert!(!balloons[1].queue.is_empty(), "should still be holding");
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
        balloons[1].queue.push_back(Bundle {
            origin_id: 1, seq: 0, created_at_round: 0, path: vec![1],
        });

        let mut stats = BundleStats::default();
        let awake: Vec<usize> = vec![0, 1];
        for round in 0..6 {
            step(&mut balloons, &adj, &awake, round, &mut stats);
        }
        assert_eq!(stats.dropped_loop, 1, "stats: {stats:?}");
        assert!(balloons.iter().all(|b| b.queue.is_empty()));
    }

    /// The whole point of the queue: a relay busy with its own bundle must still
    /// be able to accept someone else's.
    #[test]
    fn a_relay_carrying_its_own_bundle_still_accepts_transit_traffic() {
        let (mut balloons, _t, adj) = line(3);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();

        // b1 (the middle relay) is holding one of its own; b2 sends through it.
        // Only b2 is awake — if b1 also transmitted it would forward its own
        // bundle onward in the same step and the queue would net out at 1,
        // which says nothing about whether it accepted the transit bundle.
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        balloons[1].queue.push_back(Bundle {
            origin_id: 1, seq: 0, created_at_round: 0, path: vec![1],
        });
        balloons[2].queue.push_back(Bundle {
            origin_id: 2, seq: 0, created_at_round: 0, path: vec![2],
        });

        step(&mut balloons, &adj, &[2], 1, &mut stats);
        assert_eq!(balloons[1].queue.len(), 2, "relay should have accepted transit");
        assert_eq!(stats.blocked, 0);
    }

    #[test]
    fn a_full_queue_blocks_the_handoff_without_losing_the_bundle() {
        let (mut balloons, _t, adj) = line(3);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        for k in 0..RELAY_QUEUE_CAPACITY {
            balloons[1].queue.push_back(Bundle {
                origin_id: 1, seq: k as u64, created_at_round: 0, path: vec![1],
            });
        }
        balloons[2].queue.push_back(Bundle {
            origin_id: 2, seq: 0, created_at_round: 0, path: vec![2],
        });

        // b2 is awake but b1 is full: the handoff fails and b2 keeps it.
        step(&mut balloons, &adj, &[2], 1, &mut stats);
        assert_eq!(stats.blocked, 1);
        assert_eq!(balloons[2].queue.len(), 1, "sender must keep the bundle");
        assert_eq!(stats.resolved(), 0, "nothing lost: {stats:?}");
    }

    #[test]
    fn a_balloon_originates_only_one_of_its_own_at_a_time() {
        let mut balloons = vec![Balloon::new(0, 0.0, 0.0, 18000.0)];
        let adj = MeshAdjacency::default();
        let mut stats = BundleStats::default();
        // Plenty of queue room and the interval always elapsed, yet only one of
        // its own may be outstanding.
        for round in 0..10 {
            balloons[0].next_bundle_round = 0;
            step(&mut balloons, &adj, &[0], round, &mut stats);
        }
        assert_eq!(stats.originated, 1, "stats: {stats:?}");
        assert_eq!(balloons[0].queue.len(), 1);
    }

    /// A tower contact drains the queue rather than dribbling one bundle per
    /// duty cycle — the last hop is the throughput limit, so this is the one
    /// place a burst is worth spending airtime on.
    #[test]
    fn a_tower_contact_drains_up_to_a_full_contact_window() {
        let (mut balloons, _t, adj) = line(2);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        // b0 hears the tower directly and is holding a full queue.
        for k in 0..RELAY_QUEUE_CAPACITY {
            balloons[0].queue.push_back(Bundle {
                origin_id: 1, seq: k as u64, created_at_round: 0, path: vec![1, 0],
            });
        }

        step(&mut balloons, &adj, &[0], 1, &mut stats);

        let expected = TOWER_CONTACT_BUNDLES.min(RELAY_QUEUE_CAPACITY);
        assert_eq!(stats.delivered, expected as u64, "stats: {stats:?}");
        assert_eq!(balloons[0].queue.len(), RELAY_QUEUE_CAPACITY - expected);
    }

    /// The contact window applies only to towers. A relay handing off to another
    /// balloon still moves exactly one bundle per slot, because that transmission
    /// is rationed by the sender's duty cycle in the ordinary way.
    #[test]
    fn a_balloon_to_balloon_handoff_still_moves_only_one_bundle() {
        let (mut balloons, _t, adj) = line(3);
        seed_beliefs(&mut balloons, 0);
        let mut stats = BundleStats::default();
        for b in balloons.iter_mut() {
            b.next_bundle_round = u64::MAX;
        }
        for k in 0..4 {
            balloons[2].queue.push_back(Bundle {
                origin_id: 2, seq: k, created_at_round: 0, path: vec![2],
            });
        }

        step(&mut balloons, &adj, &[2], 1, &mut stats);

        assert_eq!(balloons[1].queue.len(), 1, "only one bundle should have crossed");
        assert_eq!(balloons[2].queue.len(), 3, "the rest stay with the sender");
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
