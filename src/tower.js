import * as Cesium from 'cesium';
import { BALLOON_MIN_ALT, BALLOON_MAX_ALT, params } from './config.js';
import { horizonKm, lerpColor } from './geo.js';
import { TowerModel } from './towerModel.js';

// Colors for the range gradient: green near the tower (low altitude needed,
// easy to reach), red near the outer edge (near-max altitude needed, hard).
const LOW_ALT_COLOR = [80, 200, 90];
const HIGH_ALT_COLOR = [210, 60, 40];
const CANVAS_SIZE = 256;

// ---------------------------------------------------------------------------
// Tower — a ground station. Adds a Cesium entity and range-gradient overlay
// on top of TowerModel's plain id/position data, so adding/removing a tower
// from the scene is one call each way.
// ---------------------------------------------------------------------------
export class Tower extends TowerModel {
  constructor(lon, lat, heightM) {
    super(lon, lat, heightM);
    this.entity = null;
    this.rangeCircleEntities = [];
  }

  addToScene(viewer, label) {
    this.entity = viewer.entities.add({
      position: Cesium.Cartesian3.fromDegrees(this.lon, this.lat, this.heightM),
      point: { pixelSize: 12, color: Cesium.Color.ORANGE },
      label: {
        // text: label ?? `Tower ${this.displayIndex}`,
        text: label ?? this.label,
        font: '12px sans-serif',
        pixelOffset: new Cesium.Cartesian2(0, 20),
        verticalOrigin: Cesium.VerticalOrigin.TOP,
      },
    });
    this.entity.__isTower = true;
    this.rangeCircleEntities = this._addRangeCircle(viewer);
    return this.entity;
  }

  removeFromScene(viewer) {
    if (this.entity) viewer.entities.remove(this.entity);
    this.rangeCircleEntities.forEach((e) => viewer.entities.remove(e));
    this.rangeCircleEntities = [];
  }

  // Rebuilds the range-gradient overlay in place — call this after
  // params.horizonRefractionCoeff changes, since the gradient depends on it.
  refreshRangeCircle(viewer) {
    this.rangeCircleEntities.forEach((e) => viewer.entities.remove(e));
    this.rangeCircleEntities = this._addRangeCircle(viewer);
  }

  // Builds a canvas texture whose color at each radius encodes the minimum
  // balloon altitude needed to be in range at that distance. Computed once
  // at tower creation (not per tick) — static for a fixed tower height +
  // BALLOON_MAX_ALT.
  _buildRangeGradientCanvas() {
    const towerHorizonKm = horizonKm(this.heightM);
    const maxRangeKm = towerHorizonKm + horizonKm(BALLOON_MAX_ALT);

    const canvas = document.createElement('canvas');
    canvas.width = CANVAS_SIZE;
    canvas.height = CANVAS_SIZE;
    const ctx = canvas.getContext('2d');
    const imageData = ctx.createImageData(CANVAS_SIZE, CANVAS_SIZE);
    const center = CANVAS_SIZE / 2;

    for (let y = 0; y < CANVAS_SIZE; y++) {
      for (let x = 0; x < CANVAS_SIZE; x++) {
        const dx = x - center;
        const dy = y - center;
        const pixelDist = Math.sqrt(dx * dx + dy * dy);
        const fracRadius = pixelDist / center; // 0 at tower, 1 at max range
        const idx = (y * CANVAS_SIZE + x) * 4;

        if (fracRadius > 1) {
          imageData.data[idx + 3] = 0; // outside max range: transparent
          continue;
        }

        const rangeKm = fracRadius * maxRangeKm;
        let neededAltM;
        if (rangeKm <= towerHorizonKm) {
          neededAltM = BALLOON_MIN_ALT; // reachable even at the lowest altitude
        } else {
          const balloonHorizonKm = rangeKm - towerHorizonKm;
          // Inverse of horizonKm(): horizonKm(h) = coeff * sqrt(h), so
          // h = (range / coeff)^2.
          neededAltM = Math.pow(balloonHorizonKm / params.horizonRefractionCoeff, 2);
          neededAltM = Cesium.Math.clamp(neededAltM, BALLOON_MIN_ALT, BALLOON_MAX_ALT);
        }

        const t = (neededAltM - BALLOON_MIN_ALT) / (BALLOON_MAX_ALT - BALLOON_MIN_ALT);
        const [r, g, b] = lerpColor(LOW_ALT_COLOR, HIGH_ALT_COLOR, t);
        imageData.data[idx] = r;
        imageData.data[idx + 1] = g;
        imageData.data[idx + 2] = b;
        imageData.data[idx + 3] = Math.round(0.35 * 255);
      }
    }

    ctx.putImageData(imageData, 0, 0);
    return { canvas, maxRangeM: maxRangeKm * 1000 };
  }

  // Draws the gradient disk (image-textured ellipse) plus a thin outline
  // ring marking the absolute max-range boundary.
  _addRangeCircle(viewer) {
    const { canvas, maxRangeM } = this._buildRangeGradientCanvas();
    const fillEntity = viewer.entities.add({
      position: Cesium.Cartesian3.fromDegrees(this.lon, this.lat, 0),
      ellipse: {
        semiMinorAxis: maxRangeM,
        semiMajorAxis: maxRangeM,
        height: 0,
        material: new Cesium.ImageMaterialProperty({ image: canvas, transparent: true }),
      },
    });
    const outlineEntity = viewer.entities.add({
      position: Cesium.Cartesian3.fromDegrees(this.lon, this.lat, 0),
      ellipse: {
        semiMinorAxis: maxRangeM,
        semiMajorAxis: maxRangeM,
        height: 0,
        fill: false,
        outline: true,
        outlineColor: Cesium.Color.WHITE.withAlpha(0.3),
        outlineWidth: 1,
      },
    });
    return [fillEntity, outlineEntity];
  }
}
