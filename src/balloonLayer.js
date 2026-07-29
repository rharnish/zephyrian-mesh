import * as Cesium from 'cesium';

import { balloonIconForAltitude, BALLOON_ICON_WIDTH, BALLOON_ICON_HEIGHT } from './balloonIcon.js';
import { beliefKey, deliveryKey, BELIEF_CSS, DELIVERY_CSS, DELIVERY_MARK } from './overlays.js';
import { toCesiumColors } from './cesiumColor.js';

// ---------------------------------------------------------------------------
// BalloonLayer — reconciles Cesium entities against sim-server's balloon
// snapshots, and owns everything that decides what a balloon looks like:
// which overlay is showing, which balloon is selected, and dot-vs-glyph mode.
//
// The layer holds the selection rather than mirroring it, so there is exactly
// one answer to "which balloon is selected" — the tint and the inspector can't
// disagree.
// ---------------------------------------------------------------------------

const BALLOON_COLOR = Cesium.Color.fromCssColorString('#d9dbe0');
const SELECTED_BALLOON_COLOR = Cesium.Color.fromCssColorString('#3fd0ff');
const BELIEF_COLORS = toCesiumColors(BELIEF_CSS);
const DELIVERY_COLORS = toCesiumColors(DELIVERY_CSS);

const DOT_SIZE = 6;
const SELECTED_DOT_SIZE = 11;

// Which overlay is tinting the field. Only belief tints now: delivery moved to
// a label above each balloon, which carries its own colour and so can be read
// at the same time as the tint rather than competing for it.
export const OVERLAY_NONE = 'none';
export const OVERLAY_BELIEF = 'belief';

// Label geometry. The mark sits above the balloon, so the offset depends on
// which render mode is active — a 30px billboard anchored at its base puts the
// envelope far higher than a 6px dot does.
const MARK_OFFSET_GLYPH = -34;
const MARK_OFFSET_DOT = -10;
const MARK_FONT = 'bold 13px sans-serif';

export class BalloonLayer {
  constructor() {
    this.entities = new Map(); // id -> Cesium.Entity
    // Mirrors `entities` for cheap link-line lookups, which need a position
    // per id every tick without touching entity graphics.
    this.positions = new Map(); // id -> Cesium.Cartesian3
    this.selectedId = null;
    this.overlay = OVERLAY_NONE;
    // Dots are the default; glyphs are opt-in. Authoritative in *every* scene
    // mode, 2D included — glyph detail does read as noise at flat-map zoom
    // levels, but that's a judgement for whoever is looking, not something to
    // enforce by overriding the control.
    this.useGlyphs = false;
    // Independent of `overlay` on purpose — that is the whole point of moving
    // delivery onto a label.
    this.showDeliveryMarks = false;
  }

  // The mark for one balloon, or a hidden label when the channel is unresolved
  // or marks are switched off.
  _applyMark(entity) {
    const text = DELIVERY_MARK[entity.__deliveryKey] ?? '';
    const show = this.showDeliveryMarks && text !== '';
    entity.label.show = show;
    if (!show) return;
    entity.label.text = text;
    entity.label.fillColor = DELIVERY_COLORS[entity.__deliveryKey] ?? BALLOON_COLOR;
  }

  setDeliveryMarks(on) {
    this.showDeliveryMarks = on;
    for (const entity of this.entities.values()) this._applyMark(entity);
  }

  positionOf(id) {
    return this.positions.get(id);
  }

  // Single place that decides a balloon's tint: selection wins, then whichever
  // overlay is active, then the default.
  applyColor(id) {
    const entity = this.entities.get(id);
    if (!entity) return;
    const selected = id === this.selectedId;
    let color = BALLOON_COLOR;
    if (selected) {
      color = SELECTED_BALLOON_COLOR;
    } else if (this.overlay === OVERLAY_BELIEF) {
      color = BELIEF_COLORS[entity.__beliefKey] ?? BALLOON_COLOR;
    }
    entity.point.color = color;
    entity.point.pixelSize = selected ? SELECTED_DOT_SIZE : DOT_SIZE;
    entity.billboard.color = color;
  }

  applyColorToAll() {
    for (const id of this.entities.keys()) this.applyColor(id);
  }

  setSelected(id) {
    const previous = this.selectedId;
    // Update the selection *before* recoloring, since applyColor reads it to
    // decide the tint.
    this.selectedId = id;
    if (previous !== null && previous !== id) this.applyColor(previous);
    if (id !== null) this.applyColor(id);
  }

  setOverlay(mode) {
    if (mode === this.overlay) return;
    this.overlay = mode;
    this.applyColorToAll();
  }

  setUseGlyphs(on) {
    this.useGlyphs = on;
    this.refreshRenderMode();
  }

  // Flip every existing balloon between dot and glyph rendering.
  //
  // Also re-asserted on morphComplete (wired by the caller). Strictly that's
  // redundant now that rendering no longer depends on scene mode — entity
  // graphics keep their `show` values across a morph — but it's one call per
  // morph and there is an open, unreproduced report of entity/primitive state
  // desyncing across exactly this event, so it stays as belt-and-braces.
  refreshRenderMode() {
    const showDots = !this.useGlyphs;
    const offset = new Cesium.Cartesian2(0, showDots ? MARK_OFFSET_DOT : MARK_OFFSET_GLYPH);
    for (const entity of this.entities.values()) {
      entity.point.show = showDots;
      entity.billboard.show = !showDots;
      entity.label.pixelOffset = offset;
    }
  }

  reconcile(viewer, serverBalloons) {
    const showDots = !this.useGlyphs;
    const seen = new Set();
    for (const b of serverBalloons) {
      seen.add(b.id);
      const position = Cesium.Cartesian3.fromDegrees(b.lon, b.lat, b.alt);
      this.positions.set(b.id, position);
      const icon = balloonIconForAltitude(b.alt);
      const entity = this.entities.get(b.id);
      const key = beliefKey(b);
      const dKey = deliveryKey(b);
      if (entity) {
        entity.position = position;
        entity.billboard.image = icon;
        // Only touch color when the relevant overlay state actually changed —
        // this runs for every balloon every snapshot.
        if (entity.__beliefKey !== key) {
          entity.__beliefKey = key;
          if (this.overlay === OVERLAY_BELIEF) this.applyColor(b.id);
        }
        if (entity.__deliveryKey !== dKey) {
          entity.__deliveryKey = dKey;
          this._applyMark(entity);
        }
      } else {
        const newEntity = viewer.entities.add({
          position,
          point: {
            pixelSize: DOT_SIZE,
            color: BALLOON_COLOR,
            show: showDots,
          },
          billboard: {
            image: icon,
            width: BALLOON_ICON_WIDTH,
            height: BALLOON_ICON_HEIGHT,
            color: BALLOON_COLOR,
            verticalOrigin: Cesium.VerticalOrigin.BOTTOM,
            show: !showDots,
          },
          // Outlined so the mark stays readable over both the bright globe and
          // dark space, at the few pixels it occupies when zoomed out.
          label: {
            text: '',
            font: MARK_FONT,
            fillColor: BALLOON_COLOR,
            outlineColor: Cesium.Color.BLACK,
            outlineWidth: 2,
            style: Cesium.LabelStyle.FILL_AND_OUTLINE,
            verticalOrigin: Cesium.VerticalOrigin.BOTTOM,
            pixelOffset: new Cesium.Cartesian2(0, showDots ? MARK_OFFSET_DOT : MARK_OFFSET_GLYPH),
            show: false,
          },
        });
        newEntity.__balloonId = b.id; // lets click-picking map back to a balloon id
        newEntity.__beliefKey = key;
        newEntity.__deliveryKey = dKey;
        this.entities.set(b.id, newEntity);
        this._applyMark(newEntity);
        if (b.id === this.selectedId || this.overlay !== OVERLAY_NONE) this.applyColor(b.id);
      }
    }
    for (const [id, entity] of this.entities) {
      if (!seen.has(id)) {
        viewer.entities.remove(entity);
        this.entities.delete(id);
        this.positions.delete(id);
      }
    }
  }
}
