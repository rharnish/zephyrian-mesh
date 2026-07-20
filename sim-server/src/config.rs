// Mirrors the constants in cesium-app/src/config.js that the sim loop needs.
// Keep these in sync by hand for now — see RUST_SIM_PLAN.md.

pub const BALLOON_MIN_ALT: f64 = 15000.0; // meters
pub const BALLOON_MAX_ALT: f64 = 25000.0; // meters
pub const TICK_DT_SECONDS: f64 = 1.0; // simulated seconds per tick
pub const TIME_SCALE: f64 = 60.0; // 1 real second = 60 sim seconds

pub const EARTH_RADIUS_M: f64 = 6_371_000.0;

pub const WIND_API_URL: &str = "http://127.0.0.1:8000/api/wind-levels";

// Buoyancy/ballast controller (simple target-altitude thermostat).
pub const MAX_VERTICAL_RATE: f64 = 3.0; // m/s
pub const VERTICAL_GAIN: f64 = 0.0005;
pub const TARGET_DRIFT_CHANCE_PER_TICK: f64 = 0.002;
pub const TARGET_DRIFT_RANGE: f64 = 4000.0; // meters

// Link detection throttling: recompute/broadcast edges every N ticks.
pub const LINK_UPDATE_EVERY_N_TICKS: u32 = 3;

pub const GRID_CELL_SIZE_DEG: f64 = 6.0;

pub const DEFAULT_NUM_BALLOONS: u32 = 400;
pub const DEFAULT_HORIZON_REFRACTION_COEFF: f64 = 4.12; // km per sqrt(m)

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
