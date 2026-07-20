import * as Cesium from 'cesium';
import {
  BALLOON_MIN_ALT,
  BALLOON_MAX_ALT,
  MAX_VERTICAL_RATE,
  VERTICAL_GAIN,
  TARGET_DRIFT_CHANCE_PER_TICK,
  TARGET_DRIFT_RANGE,
} from './config.js';

// ---------------------------------------------------------------------------
// Balloon model — wind-driven horizontal motion + simple buoyancy/ballast
// controller for vertical motion (target-altitude thermostat, per spec:
// "start simple, upgrade to physical model later").
// ---------------------------------------------------------------------------
export class Balloon {
  constructor(id, lon, lat, alt) {
    this.id = id;
    this.lon = lon;
    this.lat = lat;
    this.alt = alt;
    this.targetAlt = alt; // starts holding at spawn altitude
    this.entity = null;
    this.clusterId = null;
  }

  step(dtSeconds, windField) {
    const { u, v } = windField.sample(this.lon, this.lat, this.alt);

    // Horizontal: pure wind advection.
    const metersPerDegLat = 111320;
    const metersPerDegLon = 111320 * Math.cos(Cesium.Math.toRadians(this.lat));
    this.lat += (v * dtSeconds) / metersPerDegLat;
    this.lon += (u * dtSeconds) / metersPerDegLon;
    // Wrap back into [-180, 180) — otherwise a balloon crossing the
    // antimeridian drifts to lon values like 180.4 or -181, which breaks
    // anything keyed on canonical longitude (the spatial grid's cell
    // bucketing in particular).
    this.lon = ((this.lon + 180) % 360 + 360) % 360 - 180;

    // Vertical: simple target-altitude controller standing in for
    // gas/ballast physics. Proportional control, capped at MAX_VERTICAL_RATE,
    // so the balloon climbs/descends toward its target and holds there.
    if (Math.random() < TARGET_DRIFT_CHANCE_PER_TICK) {
      const delta = (Math.random() * 2 - 1) * TARGET_DRIFT_RANGE;
      this.targetAlt = Cesium.Math.clamp(this.targetAlt + delta, BALLOON_MIN_ALT, BALLOON_MAX_ALT);
    }
    const verticalRate = Cesium.Math.clamp(
      VERTICAL_GAIN * (this.targetAlt - this.alt),
      -MAX_VERTICAL_RATE,
      MAX_VERTICAL_RATE
    );
    this.alt += verticalRate * dtSeconds;
    this.alt = Cesium.Math.clamp(this.alt, BALLOON_MIN_ALT, BALLOON_MAX_ALT);
  }

  position() {
    return Cesium.Cartesian3.fromDegrees(this.lon, this.lat, this.alt);
  }

  addToScene(viewer) {
    this.entity = viewer.entities.add({
      position: this.position(),
      point: { pixelSize: 6, color: Cesium.Color.CYAN },
    });
    return this.entity;
  }
}
