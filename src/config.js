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

// Also defined in sim-server/src/config.rs (BALLOON_MIN_ALT/MAX_ALT,
// EARTH_RADIUS_M) — still read here for rendering (balloon icon scaling,
// tower range circles, horizon geometry) even though physics moved
// server-side. Kept in sync by src/config.sync.test.js.
export const BALLOON_MIN_ALT = 1000;          // meters
export const BALLOON_MAX_ALT = 25000;         // meters

export const EARTH_RADIUS = 6371000;          // meters

// Use the page's own hostname rather than hardcoding 127.0.0.1, so this
// still works when the frontend is opened from a browser on another
// computer (npm run dev --host) — 127.0.0.1 in that browser would mean the
// browser's own machine, not the one running wind_backend.py/sim-server.
// The `typeof window` guard is not defensive coding for the browser — it is
// what lets this module be imported outside one. Every tunable in this file
// lives here, so a unit test for anything that reads a tunable would
// otherwise fail on this line before reaching the code under test.
const BACKEND_HOST = typeof window !== 'undefined' ? window.location.hostname : 'localhost';

// sim-server (rust/sim-server, née rust/sim-core plan B) owns balloon/tower
// simulation state; the frontend is a thin client that renders whatever it
// broadcasts. See sim-server/README.md.
export const SIM_SERVER_URL = `http://${BACKEND_HOST}:8080`;
export const SIM_SERVER_WS_URL = `ws://${BACKEND_HOST}:8080/ws`;

// Wind field comes from sim-server, NOT directly from wind_backend.py (port
// 8000). sim-server fetches the large payload from Python once at startup and
// re-serves it here, so it's the sole client of the weather backend and the
// browser fetches wind over the same origin it already uses for everything
// else. (The browser's copy drives only the wind-vector arrows; balloon
// physics runs server-side.) See WEATHER_BACKEND_PLAN.md.
export const WIND_API_URL = `${SIM_SERVER_URL}/api/wind-levels`;
