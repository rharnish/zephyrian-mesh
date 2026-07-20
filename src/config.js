// ---------------------------------------------------------------------------
// Simulation config — tunable constants live here so the rest of the code
// doesn't have magic numbers scattered through it.
// ---------------------------------------------------------------------------

// Live-adjustable via the on-screen control panel (see main.js). Other
// modules should import `params` and read params.numBalloons /
// params.horizonRefractionCoeff rather than caching the value, so changes
// take effect without a page reload.
export const params = {
  numBalloons: 400,             // raise toward 1000+ once perf is checked
  horizonRefractionCoeff: 4.12, // km per sqrt(m); 3.57 for pure geometric LoS
};

export const BALLOON_MIN_ALT = 15000;         // meters
export const BALLOON_MAX_ALT = 25000;         // meters
export const TICK_DT_SECONDS = 1;             // simulated seconds per animation frame
export const TIME_SCALE = 60;                 // 1 real second = 60 sim seconds

export const EARTH_RADIUS = 6371000;          // meters

export const WIND_API_URL = 'http://127.0.0.1:8000/api/wind-levels';

// sim-server (rust/sim-server, née rust/sim-core plan B) owns balloon/tower
// simulation state; the frontend is a thin client that renders whatever it
// broadcasts. See sim-server/README.md.
export const SIM_SERVER_URL = 'http://127.0.0.1:8080';
export const SIM_SERVER_WS_URL = 'ws://127.0.0.1:8080/ws';

// Buoyancy/ballast controller (simple target-altitude thermostat, per spec)
export const MAX_VERTICAL_RATE = 3;           // m/s, max climb/descend speed
export const VERTICAL_GAIN = 0.0005;          // proportional gain: rate = clamp(gain * (target - alt), +-max)
export const TARGET_DRIFT_CHANCE_PER_TICK = 0.002; // chance each tick a balloon picks a new target altitude
export const TARGET_DRIFT_RANGE = 4000;       // meters, how far a new target can jump from current target

// Link detection + rendering is the most expensive part of each tick (grid
// rebuild, pairwise range checks, union-find, primitive sync). Balloons
// don't need frame-perfect link updates the way their motion needs smooth
// per-tick movement, so we only recompute/render links every N ticks.
export const LINK_UPDATE_EVERY_N_TICKS = 3;

// horizonKm(25000m) ~= 650km one-sided; two high balloons -> ~1300km max link.
// Cell size stays large to keep the 3x3 spatial-grid neighborhood correct
// at that range.
export const GRID_CELL_SIZE_DEG = 6;

// // San Francisco, California: 37.775171° N, -122.419270° W
// // Tecumseh, Michigan:        42.008933° N, -83.944386° W
// // Winthrop, Maine:           44.305000° N, -69.977000° W
// export const INITIAL_TOWERS = [
//   { lon: -122.419, lat: 37.775, heightM: 30 }, // SF, CA
//   { lon: -83.944, lat: 42.009, heightM: 30 },  // Tecumseh, MI
//   { lon: -69.977, lat: 44.305, heightM: 30 },  // Winthrop, ME
// ];

// From using Gemini chat asking about launch sites (where there could also be radio antennae)
export const INITIAL_TOWERS = [
  { lon: -122.25, lat: 37.53, heightM: 30 }, // Redwood City / Palo Alto, California
  { lon: -74.01, lat: 42.91, heightM: 30 },  // Glenville / Schenectady, New York
  { lon: -147.72, lat: 64.84, heightM: 30 }, // Fairbanks, Alaska
  { lon: -82.32, lat: 29.65, heightM: 30 },  // Gainesville, Florida
  { lon: -66.11, lat: 18.47, heightM: 30 },  // San Juan, Puerto Rico
  { lon: -23.61, lat: 14.92, heightM: 30 },  // Cidade Velha, Cabo Verde
  { lon: 128.69, lat: 36.35, heightM: 30 },  // Uiseong, South Korea
  { lon: 36.82, lat: -1.29, heightM: 30 },   // Nairobi, Kenya
  { lon: 144.79, lat: 13.48, heightM: 30 },  // Tamuning, Guam
  { lon: 55.45, lat: -20.88, heightM: 30 },  // Saint-Denis, Réunion Island
  { lon: -5.72, lat: -15.93, heightM: 30 },  // Ladder Hill, St. Helena
  { lon: 15.65, lat: 78.22, heightM: 30 }    // Longyearbyen, Svalbard (Norway)
];
