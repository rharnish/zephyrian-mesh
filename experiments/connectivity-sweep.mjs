// ---------------------------------------------------------------------------
// Radio-vs-satellite connectivity sweep.
//
// Self-contained, headless (no Cesium Viewer/WebGL/DOM) experiment. Reuses
// the app's pure simulation building blocks (SpatialGrid, UnionFind, the
// geo.js radio-range math, Balloon's motion model, TowerModel) but does NOT
// use linkDetection.js's computeGridEdges directly, since that function
// pulls render positions off `node.entity` (Cesium entities), which headless
// balloons/towers never have. Instead, computeGroundedBalloonIds() below
// re-walks the same grid-neighbor algorithm (same spatial grid, same
// precomputed-trig radio-range check, same b-/t- prefixed union-find keys
// used to avoid the balloon/tower id collision bug fixed in
// linkDetection.js) but only ever needs lon/lat/alt — never a rendered
// position — so it works identically in Node with no browser at all.
//
// Model, per balloon:
//   - every PAYLOAD_INTERVAL_SEC (5 min, fixed — not swept), a new payload
//     is generated and queued.
//   - a payload is delivered via RADIO once the balloon has been part of a
//     grounded cluster for a *continuous* streak of >= FIXED_ACK_DURATION_SEC
//     (30s — taken out of the sweep and fixed per Roy, to keep the grid
//     small enough to run in hours rather than days). On reaching that
//     streak, the ENTIRE backlog of not-yet-timed-out pending payloads is
//     flushed as radio-delivered in that same event (a real radio session
//     would transmit its whole queue, not one payload at a time) —
//     confirmed with Roy as the intended semantics.
//   - a payload that has been pending for >= fallbackTimeoutSec without
//     achieving that streak falls back to SATELLITE (always succeeds).
//
// Metric: % of all generated payloads that were delivered via radio vs.
// satellite over the run (both count as "made it to ground").
//
// Known simplification: the real wind backend isn't available in this
// environment (see wind_backend_perf memory — deferred), so this sweep uses
// zero wind. Balloons hold their spawn (lon, lat) for the whole run; only
// altitude changes (via Balloon's own target-altitude drift controller).
// That's a real modeling gap if lateral drift matters to connectivity
// dynamics — flag before trusting absolute numbers, though relative
// comparisons across the swept parameters should still be meaningful.
//
// Usage:
//   node experiments/connectivity-sweep.mjs bench                 # perf check, no sweep
//   node experiments/connectivity-sweep.mjs quick                 # tiny sweep, sanity check output shape
//   node experiments/connectivity-sweep.mjs full [out.csv]        # the real sweep (4 coeffs x 6 balloon counts x 4 timeouts = 96 combos)
// ---------------------------------------------------------------------------
import * as fs from 'node:fs';
import {
  horizonKm,
  precomputeNode,
  inRadioRangePrecomputed,
  maxPossibleRangeKm,
  randomGlobalPosition,
} from '../src/geo.js';
import { SpatialGrid } from '../src/spatialGrid.js';
import { UnionFind } from '../src/unionFind.js';
import { Balloon } from '../src/balloon.js';
import { TowerModel } from '../src/towerModel.js';
import { WindField } from '../src/windField.js';
import {
  BALLOON_MIN_ALT,
  BALLOON_MAX_ALT,
  GRID_CELL_SIZE_DEG,
  INITIAL_TOWERS,
  params,
} from '../src/config.js';

const PAYLOAD_INTERVAL_SEC = 5 * 60; // fixed, not swept
const ZERO_WIND = new WindField(
  { nx: 1, ny: 1, lo1: -180, la1: 90, lo2: 180, la2: -90, dx: 360, dy: 180 },
  [{ pressureHpa: 500, altitudeM: 18000, u_data: [[0]], v_data: [[0]] }]
);

// --- headless grounded-set computation (see file header for why this
// isn't just linkDetection.js's computeGridEdges) ---------------------------
export function computeGroundedBalloonIds(balloons, towers, grid) {
  grid.clear();
  for (const b of balloons) grid.insert(b, b.lon, b.lat);
  for (const t of towers) grid.insert(t, t.lon, t.lat);

  const allNodes = [...balloons, ...towers];
  for (const node of allNodes) precomputeNode(node, node instanceof Balloon ? node.alt : node.heightM);

  const rangeKm = maxPossibleRangeKm();
  const uf = new UnionFind();
  for (const t of towers) uf.makeSet(`t${t.id}`);
  for (const b of balloons) uf.makeSet(`b${b.id}`);

  const seenPairs = new Set();
  for (const node of allNodes) {
    const nodeKey = (node instanceof Balloon ? 'b' : 't') + node.id;
    const candidates = grid.neighbors(node.lon, node.lat, rangeKm);
    for (const other of candidates) {
      if (other === node) continue;
      const otherKey = (other instanceof Balloon ? 'b' : 't') + other.id;
      const pairKey = nodeKey < otherKey ? `${nodeKey}|${otherKey}` : `${otherKey}|${nodeKey}`;
      if (seenPairs.has(pairKey)) continue;
      seenPairs.add(pairKey);
      if (inRadioRangePrecomputed(node, other)) uf.union(nodeKey, otherKey);
    }
  }

  const groundedRoots = new Set();
  for (const t of towers) groundedRoots.add(uf.find(`t${t.id}`));
  const groundedBalloonIds = new Set();
  for (const b of balloons) {
    if (groundedRoots.has(uf.find(`b${b.id}`))) groundedBalloonIds.add(b.id);
  }
  return groundedBalloonIds;
}

export function makeBalloons(n) {
  const balloons = [];
  for (let i = 0; i < n; i++) {
    const { lon, lat } = randomGlobalPosition();
    const alt = BALLOON_MIN_ALT + Math.random() * (BALLOON_MAX_ALT - BALLOON_MIN_ALT);
    balloons.push(new Balloon(i, lon, lat, alt));
  }
  return balloons;
}

export function makeTowers() {
  return INITIAL_TOWERS.map((t) => new TowerModel(t.lon, t.lat, t.heightM));
}

// --- one simulation run for one parameter combination -----------------------
// NOTE: links are recomputed every single simulated second, unconditionally
// — NOT at a cadence tied to ackDurationSec. An earlier version tried to
// save compute by checking less often for larger ack windows, but that
// silently biased longer-ack combos toward missing brief disconnects
// (making their connectivity streaks look artificially more stable),
// directly confounding the ack-duration comparison the sweep exists to make.
export function runOne({ horizonCoeff, nBalloons, fallbackTimeoutSec, ackDurationSec, durationSec }) {
  params.horizonRefractionCoeff = horizonCoeff;

  const balloons = makeBalloons(nBalloons);
  const towers = makeTowers();
  const grid = new SpatialGrid(GRID_CELL_SIZE_DEG);

  // Per-balloon payload/connectivity state, keyed by balloon id.
  const state = balloons.map(() => ({
    pending: [], // array of createdAt (sim seconds)
    connectedSince: null, // sim time the current continuous grounded streak began, or null
  }));

  let totalPayloads = 0;
  let radioDelivered = 0;
  let satelliteDelivered = 0;

  let groundedIds = new Set();
  let nextPayloadGen = 0;

  for (let t = 0; t < durationSec; t += 1) {
    for (const b of balloons) b.step(1, ZERO_WIND);

    // Recomputed every tick (every simulated second), unconditionally — see
    // the note on runOne() above for why this must not be throttled.
    groundedIds = computeGroundedBalloonIds(balloons, towers, grid);

    if (t >= nextPayloadGen) {
      for (let i = 0; i < balloons.length; i++) {
        state[i].pending.push(t);
        totalPayloads++;
      }
      nextPayloadGen = t + PAYLOAD_INTERVAL_SEC;
    }

    for (let i = 0; i < balloons.length; i++) {
      const s = state[i];
      const grounded = groundedIds.has(balloons[i].id);

      if (grounded) {
        if (s.connectedSince === null) s.connectedSince = t;
        const streak = t - s.connectedSince;
        if (streak >= ackDurationSec && s.pending.length > 0) {
          radioDelivered += s.pending.length;
          s.pending.length = 0;
        }
      } else {
        s.connectedSince = null;
      }

      if (s.pending.length > 0) {
        const cutoff = t - fallbackTimeoutSec;
        let kept = 0;
        for (let p = 0; p < s.pending.length; p++) {
          if (s.pending[p] > cutoff) {
            s.pending[kept++] = s.pending[p];
          } else {
            satelliteDelivered++;
          }
        }
        s.pending.length = kept;
      }
    }
  }

  return { totalPayloads, radioDelivered, satelliteDelivered };
}

// --- sweep grid --------------------------------------------------------------
function range(start, end, step) {
  const out = [];
  for (let v = start; v <= end + 1e-9; v += step) out.push(Math.round(v * 1000) / 1000);
  return out;
}

const HORIZON_COEFFS = [3.4, 3.6, 3.8, 4.0]; // 4 values
const N_BALLOONS = [50, 100, 200, 400, 800, 1600]; // 6 values
const FALLBACK_TIMEOUT_MIN = [10, 20, 30, 60]; // 4 values
// ackDurationSec taken out of the sweep per Roy — fixed at 30s rather than
// varied, to cut the grid down to a size that finishes in hours, not days.
const FIXED_ACK_DURATION_SEC = 30;

function* combos() {
  for (const horizonCoeff of HORIZON_COEFFS) {
    for (const nBalloons of N_BALLOONS) {
      for (const fallbackTimeoutMin of FALLBACK_TIMEOUT_MIN) {
        yield { horizonCoeff, nBalloons, fallbackTimeoutMin, ackDurationSec: FIXED_ACK_DURATION_SEC };
      }
    }
  }
}

function runCombo(combo, durationSec) {
  const fallbackTimeoutSec = combo.fallbackTimeoutMin * 60;
  const result = runOne({
    horizonCoeff: combo.horizonCoeff,
    nBalloons: combo.nBalloons,
    fallbackTimeoutSec,
    ackDurationSec: combo.ackDurationSec,
    durationSec,
  });
  const pctRadio = result.totalPayloads > 0 ? (100 * result.radioDelivered) / result.totalPayloads : 0;
  const pctSatellite = result.totalPayloads > 0 ? (100 * result.satelliteDelivered) / result.totalPayloads : 0;
  return { ...combo, ...result, pctRadio, pctSatellite };
}

// --- per-combo JSON output -----------------------------------------------
// Every combo's result is written to its own JSON file the moment it
// finishes (RESULTS_DIR below) — so a long run can be killed/resumed and
// nothing already-computed is lost, unlike rewriting one big CSV in place.
// A separate `combine` mode reads every JSON file here and produces the
// final CSV.
const RESULTS_DIR = 'experiments/results';

export function comboFileName(combo) {
  const h = combo.horizonCoeff.toFixed(2).replace('.', 'p');
  return `h${h}_n${combo.nBalloons}_t${combo.fallbackTimeoutMin}_a${combo.ackDurationSec}.json`;
}

export function writeComboJson(combo, row) {
  fs.mkdirSync(RESULTS_DIR, { recursive: true });
  const path = `${RESULTS_DIR}/${comboFileName(combo)}`;
  fs.writeFileSync(path, JSON.stringify(row, null, 2));
  return path;
}

export function combineJsonToCsv(outPath) {
  const files = fs.readdirSync(RESULTS_DIR).filter((f) => f.endsWith('.json'));
  const rows = files.map((f) => JSON.parse(fs.readFileSync(`${RESULTS_DIR}/${f}`, 'utf8')));
  rows.sort((a, b) =>
    a.horizonCoeff - b.horizonCoeff || a.nBalloons - b.nBalloons || a.fallbackTimeoutMin - b.fallbackTimeoutMin
  );
  const header = 'horizonCoeff,nBalloons,fallbackTimeoutMin,ackDurationSec,totalPayloads,radioDelivered,satelliteDelivered,pctRadio,pctSatellite\n';
  const lines = rows.map((r) =>
    [r.horizonCoeff, r.nBalloons, r.fallbackTimeoutMin, r.ackDurationSec, r.totalPayloads, r.radioDelivered, r.satelliteDelivered, r.pctRadio.toFixed(3), r.pctSatellite.toFixed(3)].join(',')
  );
  fs.writeFileSync(outPath, header + lines.join('\n') + '\n');
  return rows.length;
}

// --- entry points ------------------------------------------------------------
// Guarded so this file can also be `import()`-ed (e.g. by a validation
// script) without immediately kicking off a CLI run as a side effect.
const isMain = import.meta.url === `file://${process.argv[1]}`;
const mode = process.argv[2] || 'bench';

if (isMain && mode === 'bench') {
  // Worst-case single combo (max balloons) over a short duration, to
  // extrapolate full-sweep cost before committing to it.
  const durationSec = 60 * 60; // 1 sim hour
  const combo = { horizonCoeff: 4.0, nBalloons: Math.max(...N_BALLOONS), fallbackTimeoutMin: 30, ackDurationSec: FIXED_ACK_DURATION_SEC };
  console.log(`Benchmarking worst case (n=${combo.nBalloons}, ackDurationSec=${combo.ackDurationSec}) over ${durationSec / 3600} sim hour(s)...`);
  const t0 = Date.now();
  const result = runCombo(combo, durationSec);
  const wallSec = (Date.now() - t0) / 1000;
  console.log(`Result: ${JSON.stringify(result)}`);
  console.log(`Wall time: ${wallSec.toFixed(1)}s for 1 sim hour => ${(wallSec * 24).toFixed(0)}s (${(wallSec * 24 / 60).toFixed(1)} min) per 24-sim-hour combo at worst case.`);
  console.log(`Total combos in full sweep: ${HORIZON_COEFFS.length * N_BALLOONS.length * FALLBACK_TIMEOUT_MIN.length}`);
} else if (isMain && mode === 'quick') {
  // Tiny sanity sweep: a couple values per dimension, short duration, to
  // check the output shape and that numbers look plausible before trusting
  // the model.
  const durationSec = 2 * 60 * 60; // 2 sim hours
  const rows = [];
  for (const nBalloons of [50, 200]) {
    for (const horizonCoeff of [3.4, 4.0]) {
      const combo = { horizonCoeff, nBalloons, fallbackTimeoutMin: 20, ackDurationSec: FIXED_ACK_DURATION_SEC };
      rows.push(runCombo(combo, durationSec));
    }
  }
  console.table(rows.map((r) => ({
    nBalloons: r.nBalloons,
    horizonCoeff: r.horizonCoeff,
    totalPayloads: r.totalPayloads,
    pctRadio: r.pctRadio.toFixed(1),
    pctSatellite: r.pctSatellite.toFixed(1),
  })));
} else if (isMain && mode === 'full') {
  // Optional sharding for parallel execution: `full <shardIndex> <shardCount>`
  // launches N processes side by side (see run-shards.sh), each handling
  // every shardCount-th combo. Every combo writes its own JSON result file
  // to RESULTS_DIR the moment it finishes — combos already present on disk
  // are skipped, so a killed/restarted run resumes instead of redoing work.
  const shardIndex = process.argv[3] !== undefined ? parseInt(process.argv[3], 10) : 0;
  const shardCount = process.argv[4] !== undefined ? parseInt(process.argv[4], 10) : 1;
  const durationSec = 24 * 60 * 60; // 24 sim hours, per Roy's chosen tradeoff
  const all = [...combos()].filter((_, i) => i % shardCount === shardIndex);
  console.log(`Running full sweep shard ${shardIndex}/${shardCount}: ${all.length} combinations x ${durationSec / 3600} sim hours each.`);
  console.log(`Per-combo results land in ${RESULTS_DIR}/ as they finish. Run "combine" afterward to build the final CSV.`);
  fs.mkdirSync(RESULTS_DIR, { recursive: true });
  let done = 0;
  const t0 = Date.now();
  for (let i = 0; i < all.length; i++) {
    const combo = all[i];
    const path = `${RESULTS_DIR}/${comboFileName(combo)}`;
    if (fs.existsSync(path)) {
      console.log(`[shard ${shardIndex}] [${i + 1}/${all.length}] already done, skipping: ${path}`);
      continue;
    }
    const row = runCombo(combo, durationSec);
    writeComboJson(combo, row);
    done++;
    const elapsed = (Date.now() - t0) / 1000;
    const rate = done / elapsed;
    const etaSec = rate > 0 ? (all.length - (i + 1)) / rate : 0;
    console.log(`[shard ${shardIndex}] [${i + 1}/${all.length}] wrote ${path} — elapsed ${elapsed.toFixed(0)}s, ETA ${(etaSec / 60).toFixed(1)} min`);
  }
  console.log(`Shard ${shardIndex} done.`);
} else if (isMain && mode === 'combine') {
  const outPath = process.argv[3] || 'experiments/connectivity-sweep-results.csv';
  const n = combineJsonToCsv(outPath);
  console.log(`Combined ${n} result file(s) from ${RESULTS_DIR}/ into ${outPath}`);
} else if (isMain) {
  console.error(`Unknown mode "${mode}". Use: bench | quick | full [shardIndex] [shardCount] | combine [outPath]`);
  process.exit(1);
}
