import * as Cesium from 'cesium';
import { EARTH_RADIUS, BALLOON_MAX_ALT, params } from './config.js';

// ---------------------------------------------------------------------------
// Radio line-of-sight
// ---------------------------------------------------------------------------
export function horizonKm(heightM) {
  return params.horizonRefractionCoeff * Math.sqrt(Math.max(heightM, 0));
}

// The largest possible link range right now: two nodes both at the highest
// possible altitude a balloon can be. Towers are always lower than
// BALLOON_MAX_ALT, so this is a safe (if slightly generous) upper bound for
// ANY pair, balloon-balloon or balloon-tower. Used to size spatial-grid
// neighbor searches so they can't miss a valid pair. Recomputed from the
// live params each call, since horizonRefractionCoeff is UI-adjustable.
export function maxPossibleRangeKm() {
  return 2 * horizonKm(BALLOON_MAX_ALT);
}

// Precompute a node's trig/horizon once per tick so repeated pairwise
// comparisons (each node is checked against ~9 grid cells of neighbors)
// don't redo sin/cos/sqrt from scratch every time. Mutates the node,
// stashing the values under underscore-prefixed fields.
export function precomputeNode(node, heightM) {
  node._latRad = Cesium.Math.toRadians(node.lat);
  node._lonRad = Cesium.Math.toRadians(node.lon);
  node._sinLat = Math.sin(node._latRad);
  node._cosLat = Math.cos(node._latRad);
  node._horizonKm = horizonKm(heightM);
}

// Great-circle distance using each node's precomputed trig (see precomputeNode).
export function greatCircleDistanceKmPrecomputed(a, b) {
  const R = EARTH_RADIUS / 1000;
  const dPhi = b._latRad - a._latRad;
  const dLambda = b._lonRad - a._lonRad;
  const sinDPhi = Math.sin(dPhi / 2);
  const sinDLambda = Math.sin(dLambda / 2);
  const h = sinDPhi * sinDPhi + a._cosLat * b._cosLat * sinDLambda * sinDLambda;
  return 2 * R * Math.asin(Math.sqrt(h));
}

export function inRadioRangePrecomputed(a, b) {
  const maxRange = a._horizonKm + b._horizonKm;
  return greatCircleDistanceKmPrecomputed(a, b) <= maxRange;
}

// ---------------------------------------------------------------------------
// Uniform random point on a sphere (avoids clustering near the poles that a
// naive uniform-lat sample would produce).
// ---------------------------------------------------------------------------
export function randomGlobalPosition() {
  const lon = Math.random() * 360 - 180;
  const lat = Cesium.Math.toDegrees(Math.asin(Math.random() * 2 - 1));
  return { lon, lat };
}

export function lerpColor(c1, c2, t) {
  const clamped = Cesium.Math.clamp(t, 0, 1);
  return [
    Math.round(c1[0] + (c2[0] - c1[0]) * clamped),
    Math.round(c1[1] + (c2[1] - c1[1]) * clamped),
    Math.round(c1[2] + (c2[2] - c1[2]) * clamped),
  ];
}
