// Mirrors the constants in cesium-app/src/config.js that the sim loop needs.
// Keep these in sync by hand for now — see RUST_SIM_PLAN.md.

pub const BALLOON_MIN_ALT: f64 = 1000.0; // meters
pub const BALLOON_MAX_ALT: f64 = 25000.0; // meters
pub const TICK_DT_SECONDS: f64 = 1.0; // simulated seconds per tick
pub const TIME_SCALE: f64 = 15.0; // 1 real second = 15 sim seconds

pub const EARTH_RADIUS_M: f64 = 6_371_000.0;

pub const WIND_API_URL: &str = "http://127.0.0.1:8000/api/wind-levels";

// Buoyancy/ballast controller (simple target-altitude thermostat).
pub const MAX_VERTICAL_RATE: f64 = 3.0; // m/s
pub const VERTICAL_GAIN: f64 = 0.0005;
/// Rate at which a balloon picks a new target altitude, per *simulated* hour.
///
/// Deliberately not per-tick. A tick is TICK_DT_SECONDS * TIME_SCALE simulated
/// seconds, so a per-tick probability silently rescales this whenever TIME_SCALE
/// moves: lowering TIME_SCALE from 60 to 15 would have quadrupled how often
/// balloons retarget per simulated hour, and since altitude churn is what breaks
/// radio links, that would have shown up as a mysteriously higher stale-belief
/// fraction. Expressed as a rate, retargeting means the same thing physically at
/// any TIME_SCALE.
///
/// 0.12/h ≈ one retarget every ~8 simulated hours, matching the original
/// 0.002-per-tick behaviour at TIME_SCALE = 60.
pub const TARGET_DRIFT_CHANCE_PER_SIM_HOUR: f64 = 0.12;
pub const TARGET_DRIFT_RANGE: f64 = 4000.0; // meters

// Link detection throttling: recompute/broadcast edges every N ticks.
pub const LINK_UPDATE_EVERY_N_TICKS: u32 = 3;

// --- Comms clock ------------------------------------------------------------
//
// The comms protocol steps on its own divisor of the tick clock, exactly like
// LINK_UPDATE_EVERY_N_TICKS above. This exists because TICK_INTERVAL_MS has to
// serve two unrelated jobs: it is the snapshot rate (which must stay fast — the
// client sets balloon positions directly per snapshot, with no interpolation,
// so anything much below 20 Hz visibly stutters) *and* it was the protocol
// clock. Denominating comms in ticks therefore locked the whole protocol to
// 1200x real time: the measured discovery arc crossed the planet
// in 2.4 real seconds, and belief expiry in 3. Nothing was observable.
//
// With comms on its own clock the protocol's real-time pace is tunable without
// touching physics smoothness. All the constants below are in *rounds*, not
// ticks; one round is COMMS_EVERY_N_TICKS ticks.
pub const COMMS_EVERY_N_TICKS: u64 = 8;

/// Simulated seconds in one comms round — for reading the constants below as
/// physical durations rather than counts.
pub const COMMS_ROUND_SIM_SECONDS: f64 = COMMS_EVERY_N_TICKS as f64 * TICK_DT_SECONDS * TIME_SCALE;

// --- Beacon-based connectivity discovery (see MESH_COMMS_DESIGN.md §1).
//
// What makes discovery slow here is radio *duty cycling*, not propagation
// delay: links survive for hours of sim time (a balloon drifts ~0.4% of link
// range per link round), so if nodes transmitted continuously every belief
// would instantly equal ground truth and there'd be nothing to simulate. Real
// HAB radios can't afford that — they wake, beacon, and sleep.
/// Nominal gap between a node's beacon transmissions, in comms rounds.
pub const BEACON_INTERVAL_ROUNDS: u64 = 5;
/// +/- jitter on that gap, so nodes don't fall into lockstep.
pub const BEACON_JITTER_ROUNDS: u64 = 1;
/// Maximum age of the *news*, measured from when the tower emitted the wave —
/// not from when this balloon last heard it repeated. Keying expiry on when a
/// belief was last heard does not work: any relay of stale news renews its
/// lease, so a cluster cut off from every tower sustains dead routes forever
/// (see the beacon.rs tests, and bin/beacon_convergence.rs phase 3).
///
/// This is OSPF's LSA MaxAge idea. The floor on it is propagation time: a
/// belief has to survive long enough to reach the far end of the mesh, or deep
/// balloons expire it on arrival and can never hold a route. Measured
/// convergence is ~38 rounds to reach 99% of a 1200-balloon field, so this must
/// sit above that — it is ~12 beacon intervals, not the 3-4 a contact-recency
/// timeout would want. **Not a pacing dial**: lower it toward the convergence
/// figure and deep balloons expire beliefs on arrival. Retune the comms clock
/// instead.
pub const BELIEF_MAX_AGE_ROUNDS: u64 = 60;
/// Stop rebroadcasting past this depth. Paths this long only occur near the
/// percolation threshold, where handing off to satellite is the right policy
/// anyway (see mesh_depth.rs). Also bounds distance-vector count-to-infinity.
pub const BEACON_MAX_HOPS: u32 = 20;

// --- Telemetry bundles (C2, see MESH_COMMS_DESIGN.md §1 and §4) -------------
//
// Bundles advance one hop per *duty-cycle slot*, not per round: a bundle moves
// only when the balloon holding it wakes to transmit, which is the same slot it
// beacons on. So one hop costs BEACON_INTERVAL_ROUNDS rounds, and a path of
// depth d takes d * BEACON_INTERVAL_ROUNDS rounds one way.
//
/// How often a balloon originates a telemetry bundle (200 rounds ≈ 6.7 sim
/// hours). Measured, not chosen: at 25 the mesh gridlocks — every balloon is
/// permanently carrying, so nobody can relay, and delivery falls to 29% with
/// 81k blocked handoffs. Backing off to 200 leaves in-flight at ~half the fleet
/// and raises delivery to 41%. See bin/bundle_delivery.rs.
///
/// **This is the demand knob, and for a long time it was the demand knob on a
/// saturated resource.** Measured under the old one-bundle-per-contact rule:
/// only ~23 of 1200 balloons can hear a tower at any moment, giving a ceiling of
/// ~4.7 deliveries/round, and sweeping this constant 50 -> 1600 moved offered
/// load 28x while delivery stayed pinned near 3.1/round — completion moved
/// 16% -> 93% purely because the denominator changed. Queues, TTLs and route
/// selection were all measured and none of them moved it.
///
/// **That ceiling no longer binds at this interval.** With
/// TOWER_CONTACT_BUNDLES = 4 the last hop can pass ~16.6/round against the
/// ~5.9/round this constant offers, so the demand sweep above would come out
/// differently today and should be re-run before being quoted. 200 is kept
/// because it puts the mesh visibly under load without being hopeless.
/// See docs/bundle-delivery-report.html §5–§7 and MESH_COMMS_DESIGN.md §4.
pub const BUNDLE_INTERVAL_ROUNDS: u64 = 200;
/// How long a bundle may go unresolved before it is given up on. Must exceed a
/// deep-path traversal (14 hops x BEACON_INTERVAL_ROUNDS = 70 rounds) or bundles
/// expire while legitimately in flight. In slice 2 this becomes the ack timeout
/// that hands the bundle to satellite rather than dropping it.
pub const BUNDLE_MAX_AGE_ROUNDS: u64 = 150;
/// How many bundles a balloon may hold at once — its store-and-forward buffer.
///
/// Separate from the one-outstanding-bundle rule on *origination*. Conflating
/// the two was a design error: with a single slot a balloon holding its own
/// bundle cannot relay anyone else's. Origination stays capped at one
/// outstanding per balloon; this governs transit traffic.
///
/// **What bounds this physically is not memory, and not energy directly.** A
/// telemetry bundle is a few hundred bytes, so even a small MCU holds thousands
/// — storage is free. Energy is the real constraint on the platform, but it
/// limits *transmitting*, not *holding*: a LoRa transmit burst draws ~120 mA
/// while SRAM retention costs microamps. That constraint is already modelled, as
/// BEACON_INTERVAL_ROUNDS — the duty cycle — not as queue depth.
///
/// The bound that does apply is derived from the other two constants. A balloon
/// drains one bundle per duty-cycle slot, so anything sitting deeper than
/// BUNDLE_MAX_AGE_ROUNDS / BEACON_INTERVAL_ROUNDS = 30 positions expires before
/// its turn ever comes. A queue deeper than that is unreachable by construction.
///
/// Measured (bin/bundle_delivery.rs, 1200 balloons, degree ~6): raising this
/// 1 -> 8 cuts blocked handoffs from 25.8k to 2.3k but leaves delivery flat at
/// 39-45%. So the queue does what it should and is *not* the delivery
/// bottleneck — the earlier diagnosis was wrong, and blocking was a symptom
/// rather than the cause.
///
/// Re-measured after TOWER_CONTACT_BUNDLES landed, since a tower contact now
/// drains 4 at once and the queue empties differently:
///
/// | capacity | 1 | 2 | 4 | 8 | 16 |
/// |---|---|---|---|---|---|
/// | blocked % of slots | 78.5 | 44.9 | 27.5 | 7.9 | 2.8 |
/// | completion | 39.2% | 72.3% | 60.2% | 80.2% | 64.3% |
///
/// **Read the blocked row, not the completion row.** Completion here is
/// non-monotone because it is dominated by how many balloons happened to be in
/// tower range that run — across these five runs that count varied 15.0 to 25.8
/// and correlates with delivered/round at r = 0.94, which swamps any queue
/// effect at single-seed. Blocking is the unconfounded signal, and it is
/// monotone. So: 1 is clearly too small; above 2 this sweep cannot resolve a
/// difference in delivery. 8 is kept because it takes blocking to ~8% at no
/// measured cost, and stays well under the reachable depth above.
pub const RELAY_QUEUE_CAPACITY: usize = 8;
/// How many bundles a balloon may hand to a tower in a single contact.
///
/// One transmission per wake slot is the right rule for a *beacon*: it is a
/// broadcast to no one in particular, and the duty cycle is what rations it.
/// A tower contact is a different event — a point-to-point link to a station
/// with mains power and a real antenna — and spending the whole airtime budget
/// of one wake on it is both physically reasonable (a few hundred bytes per
/// bundle; 8 of them is a short burst) and the only lever that acts directly on
/// the measured bottleneck.
///
/// Set to 1 this reproduces the old one-bundle-per-slot behaviour exactly, which
/// is how the two were compared (see docs/bundle-delivery-report.html §5): delivery
/// is capped by the ~23 balloons that can hear a tower, so the last hop is the
/// only place throughput can come from.
///
/// Measured (1200 balloons, coeff 4.12, 2000 rounds), completion rate:
///
/// | window | 1 | 2 | 4 | 8 |
/// |---|---|---|---|---|
/// | completion | 51.2% | 60.7% | **67.4%** | 66.0% |
///
/// **4, not 8** — the gain saturates because past ~4 the last hop stops being
/// the binding constraint (ceiling 16.6/round against 5.9/round offered) and the
/// limit moves back into the mesh, to bundles expiring before they ever reach a
/// tower-adjacent balloon. 8 buys nothing measurable and claims more airtime.
pub const TOWER_CONTACT_BUNDLES: usize = 4;
/// Hop budget. A bundle whose recorded path reaches this length is dropped.
/// Sized against p95 mesh depth (see mesh_depth.rs); deeper paths only exist
/// near percolation, where satellite is the right answer anyway.
pub const BUNDLE_MAX_HOPS: usize = 20;

pub const GRID_CELL_SIZE_DEG: f64 = 6.0;

pub const DEFAULT_NUM_BALLOONS: u32 = 400;
pub const DEFAULT_HORIZON_REFRACTION_COEFF: f64 = 4.12; // km per sqrt(m)

/// Full pool that's always simulated. The slider only changes how many of
/// these are included in connectivity detection and sent to the client —
/// it no longer respawns anything.
pub const BALLOON_POOL_SIZE: u32 = 2000;

// How often (in ticks) to broadcast a snapshot to connected clients, even on
// non-link ticks (balloon positions still need to look smooth every tick).
pub const TICK_INTERVAL_MS: u64 = 1000 / 20; // 20 ticks/sec wall-clock

// Mirrors INITIAL_TOWERS in cesium-app/src/config.js — (lon, lat, heightM).
// Seeded into the World at startup instead of created client-side, since the
// server now owns tower state.
pub const INITIAL_TOWERS: &[(f64, f64, f64)] = &[
    (-122.25, 37.53, 30.0),  // Redwood City / Palo Alto, California
    (-74.01, 42.91, 30.0),   // Glenville / Schenectady, New York
    (-147.72, 64.84, 30.0),  // Fairbanks, Alaska
    (-82.32, 29.65, 30.0),   // Gainesville, Florida
    (-66.11, 18.47, 30.0),   // San Juan, Puerto Rico
    (-23.61, 14.92, 30.0),   // Cidade Velha, Cabo Verde
    (128.69, 36.35, 30.0),   // Uiseong, South Korea
    (36.82, -1.29, 30.0),    // Nairobi, Kenya
    (144.79, 13.48, 30.0),   // Tamuning, Guam
    (55.45, -20.88, 30.0),   // Saint-Denis, Réunion Island
    (-5.72, -15.93, 30.0),   // Ladder Hill, St. Helena
    (15.65, 78.22, 30.0),    // Longyearbyen, Svalbard (Norway)
];
