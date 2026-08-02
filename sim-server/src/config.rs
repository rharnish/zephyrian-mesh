// Mirrors the constants in cesium-app/src/config.js that the sim loop needs.
// Most of this file has no JS counterpart anymore (physics moved server-side —
// see docs/history/RUST_SIM_PLAN.md). BALLOON_MIN_ALT, BALLOON_MAX_ALT, and
// EARTH_RADIUS_M are the exception: config.js still reads matching values for
// rendering, kept in sync by hand and checked by cesium-app/src/config.sync.test.js.

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

// --- Comms protocol tunables have moved -----------------------------------
//
// Everything that used to sit here — beacon intervals, belief expiry, bundle
// timeouts, queue capacities, the tower-contact window, the log bound — now
// lives on `protocol::dv_dtn::params::DvDtnParams`, because those are that
// protocol's constants rather than the simulation's. A protocol that doesn't
// flood from towers has no beacon interval; one that replicates instead of
// routing has no belief to expire.
//
// The comms *clock* above stays here: every protocol needs one, and it is the
// simulation that decides how a round maps onto ticks and simulated seconds.

// Humidity synthesis (atmosphere.rs). Not ISA — the ISA says nothing about
// humidity and the ERA5 dataset behind the wind field is wind-only, so §1 of
// the design calls for a synthesized decreasing-with-altitude profile.
/// Relative humidity at sea level, percent.
pub const RH_SURFACE_PCT: f64 = 70.0;
/// e-folding height for humidity decay. ~3 km puts the stratosphere under 1% RH,
/// which is the right order for air that has been wrung out crossing the
/// tropopause cold trap.
pub const RH_SCALE_HEIGHT_M: f64 = 3000.0;
/// Amplitude of the smooth spatial variation layered on the profile, as a
/// fraction. Enough that balloons in different places report visibly different
/// humidity; small enough that the altitude profile still dominates.
pub const RH_SPATIAL_AMPLITUDE: f64 = 0.15;

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
