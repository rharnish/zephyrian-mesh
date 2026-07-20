import { precomputeNode, inRadioRangePrecomputed, maxPossibleRangeKm } from './geo.js';
import { Balloon } from './balloon.js';

// Balloon ids are array indices reused on every respawn (0..N-1) and tower
// ids come from a separate counter that also starts at 0 — so a bare
// String(id) collides between the two node types (balloon 0 and tower 0
// both stringify to "0"). Every place an id is turned into a map/set key
// must go through this prefixed key instead, so the two id spaces can never
// collide.
export function nodeKey(node) {
  return node instanceof Balloon ? `b${node.id}` : `t${node.id}`;
}

function pairKeyFor(aKey, bKey) {
  return aKey < bKey ? `${aKey}|${bKey}` : `${bKey}|${aKey}`;
}

// ---------------------------------------------------------------------------
// The ACTUAL production edge finder: spatial grid + precomputed trig. This
// is the exact function the tick loop calls — verification below runs this
// same function, not a separate reimplementation, so a bug here shows up
// in both places rather than being silently missed by testing a copy.
// ---------------------------------------------------------------------------
export function computeGridEdges(balloons, towers, grid, currentTime) {
  grid.clear();
  for (const b of balloons) grid.insert(b, b.lon, b.lat);
  for (const t of towers) grid.insert(t, t.lon, t.lat);

  const allNodes = [...balloons, ...towers];
  for (const node of allNodes) {
    precomputeNode(node, node instanceof Balloon ? node.alt : node.heightM);
  }

  const rangeKm = maxPossibleRangeKm();
  const edgesByPairKey = new Map(); // pairKey -> {aKey, bKey, posA, posB}
  const seenPairs = new Set();
  for (const node of allNodes) {
    const candidates = grid.neighbors(node.lon, node.lat, rangeKm);
    for (const other of candidates) {
      if (other === node) continue;
      const pairKey = pairKeyFor(nodeKey(node), nodeKey(other));
      if (seenPairs.has(pairKey)) continue;
      seenPairs.add(pairKey);

      if (inRadioRangePrecomputed(node, other)) {
        edgesByPairKey.set(pairKey, {
          aKey: nodeKey(node),
          bKey: nodeKey(other),
          posA: node.entity.position.getValue(currentTime),
          posB: other.entity.position.getValue(currentTime),
        });
      }
    }
  }
  return edgesByPairKey;
}

// ---------------------------------------------------------------------------
// Ground truth: O(n^2), no spatial grid, no throttling — checks literally
// every pair. Slow (don't run every tick at high balloon counts), but
// correct by construction, so it's a trustworthy reference to diff against.
// ---------------------------------------------------------------------------
export function bruteForceEdgeKeys(balloons, towers) {
  const allNodes = [...balloons, ...towers];
  for (const node of allNodes) {
    precomputeNode(node, node instanceof Balloon ? node.alt : node.heightM);
  }

  const keys = new Set();
  for (let i = 0; i < allNodes.length; i++) {
    for (let j = i + 1; j < allNodes.length; j++) {
      if (inRadioRangePrecomputed(allNodes[i], allNodes[j])) {
        keys.add(pairKeyFor(nodeKey(allNodes[i]), nodeKey(allNodes[j])));
      }
    }
  }
  return keys;
}

// One-shot check: runs the production grid algorithm and brute-force ground
// truth back-to-back against the SAME node positions (no balloon movement
// between the two), so any difference is a genuine algorithm bug, not lag.
// Logs a summary and returns the details.
export function verifyEdgesOnce(balloons, towers, grid, currentTime) {
  const gridEdges = computeGridEdges(balloons, towers, grid, currentTime);
  const groundTruth = bruteForceEdgeKeys(balloons, towers);
  const gridKeys = new Set(gridEdges.keys());

  const missed = [...groundTruth].filter((k) => !gridKeys.has(k));   // false negatives: real bug
  const spurious = [...gridKeys].filter((k) => !groundTruth.has(k)); // false positives: real bug

  console.log(
    `[verifyEdgesOnce] ground truth: ${groundTruth.size} edges, grid algorithm: ${gridKeys.size} edges, ` +
    `missed: ${missed.length}, spurious: ${spurious.length}`
  );
  if (missed.length > 0) {
    console.log('[verifyEdgesOnce] MISSED (grid algorithm failed to find these, but they are real):', missed);
  }
  if (spurious.length > 0) {
    console.log('[verifyEdgesOnce] SPURIOUS (grid algorithm found these, but they are NOT actually in range):', spurious);
  }
  if (missed.length === 0 && spurious.length === 0) {
    console.log('[verifyEdgesOnce] grid algorithm matches ground truth exactly.');
  }
  return { groundTruth, gridKeys, missed, spurious };
}

// ---------------------------------------------------------------------------
// Continuous transient-edge monitor: catches the OTHER failure mode, where
// link detection itself is correct at each snapshot, but throttling
// (only recomputing links every N ticks) means a brief pass-through never
// gets sampled at all. Call sampleEveryTick() on every physics tick
// (cheap-ish; it's still O(n^2), so this is a debug-only tool, not
// something to leave on by default) and checkSync() whenever the throttled
// link update actually runs. Anything seen by sampleEveryTick() since the
// last checkSync() but absent from the synced result was a real, if brief,
// in-range event that the throttled pipeline never rendered a link for.
// ---------------------------------------------------------------------------
export class TransientEdgeMonitor {
  constructor() {
    this.seenSinceLastSync = new Set();
  }

  sampleEveryTick(balloons, towers) {
    const keys = bruteForceEdgeKeys(balloons, towers);
    for (const k of keys) this.seenSinceLastSync.add(k);
  }

  checkSync(syncedEdgeKeys) {
    const synced = new Set(syncedEdgeKeys);
    const missedTransients = [...this.seenSinceLastSync].filter((k) => !synced.has(k));
    if (missedTransients.length > 0) {
      console.log(
        `[TransientEdgeMonitor] ${missedTransients.length} edge(s) were true at some point since the ` +
        `last sync but never appeared in a rendered link (throttling missed a brief pass-through):`,
        missedTransients
      );
    }
    this.seenSinceLastSync = new Set();
  }
}
