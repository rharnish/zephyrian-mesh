// Balloon glyph canvases: a teardrop envelope whose fullness tracks altitude.
//
// No Cesium import — the canvases are handed to a billboard as plain images,
// and the altitude-to-glyph lookup is arithmetic. That keeps the index
// selection unit-testable without a browser or a Cesium build.

import { BALLOON_MIN_ALT, BALLOON_MAX_ALT } from './config.js';

// Balloon icon layout, shared between drawing and billboard anchoring. The
// basket sits at the very bottom of the canvas so a BOTTOM-origin billboard
// places the basket — not the envelope — at the entity's actual position,
// which is also where radio-link edges terminate: edges visually connect
// basket to basket, not balloon-envelope to balloon-envelope.
export const BALLOON_ICON_WIDTH = 16;
export const BALLOON_ICON_HEIGHT = 30;
const BALLOON_BASKET_TOP_Y = 25;
const BALLOON_BASKET_HEIGHT = 4;

const BALLOON_ICON_COUNT = 10;

// Draws a hot-air-balloon glyph (teardrop envelope + single rigging line +
// basket) onto a canvas. `fullness` (0..1) controls the envelope shape: 0 is
// a narrow, elongated teardrop (low-altitude balloon, not yet fully
// inflated), 1 is a fuller, rounder teardrop (high-altitude balloon at max
// inflation). Rendered in white so it can be recolored per-entity via
// billboard.color (Cesium multiplies the image by that tint).
function buildBalloonIcon(fullness) {
  const canvas = document.createElement('canvas');
  canvas.width = BALLOON_ICON_WIDTH;
  canvas.height = BALLOON_ICON_HEIGHT;
  const ctx = canvas.getContext('2d');

  ctx.fillStyle = '#ffffff';
  ctx.strokeStyle = '#ffffff';
  ctx.lineWidth = 1;

  const cx = BALLOON_ICON_WIDTH / 2;
  const topY = 2;
  const rx = 4 + 3 * fullness; // envelope bulge half-width: 4..7
  const bulgeCenterY = topY + rx;
  const bulgeBottomY = bulgeCenterY + rx;
  const tipY = bulgeBottomY + (12 - 9 * fullness); // point length: long/narrow at low fullness, short/round at high fullness

  // Teardrop: rounded top (semicircle) tapering to a point at tipY.
  ctx.beginPath();
  ctx.arc(cx, bulgeCenterY, rx, Math.PI, 0, false);
  ctx.quadraticCurveTo(cx + rx * 0.3, bulgeBottomY, cx, tipY);
  ctx.quadraticCurveTo(cx - rx * 0.3, bulgeBottomY, cx - rx, bulgeCenterY);
  ctx.closePath();
  ctx.fill();

  // Single rigging line from the envelope's point down to the basket.
  ctx.beginPath();
  ctx.moveTo(cx, tipY);
  ctx.lineTo(cx, BALLOON_BASKET_TOP_Y);
  ctx.stroke();

  // Basket
  ctx.fillRect(cx - 2, BALLOON_BASKET_TOP_Y, 4, BALLOON_BASKET_HEIGHT);

  return canvas;
}

// Built on first use rather than at import, so importing this module doesn't
// require a DOM — which is what lets the index arithmetic below be tested in
// Node. Ten small canvases; the cost lands on the first balloon rendered.
let balloonIcons = null;

// Which of the precomputed glyphs an altitude maps to. Exported separately
// from the canvas lookup because it is the only part with any logic in it.
export function balloonIconIndexForAltitude(altM) {
  const span = BALLOON_MAX_ALT - BALLOON_MIN_ALT;
  const t = Math.min(1, Math.max(0, (altM - BALLOON_MIN_ALT) / span));
  return Math.round(t * (BALLOON_ICON_COUNT - 1));
}

export function balloonIconForAltitude(altM) {
  if (!balloonIcons) {
    balloonIcons = Array.from({ length: BALLOON_ICON_COUNT }, (_, i) =>
      buildBalloonIcon(i / (BALLOON_ICON_COUNT - 1))
    );
  }
  return balloonIcons[balloonIconIndexForAltitude(altM)];
}
