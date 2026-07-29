import 'cesium/Build/Cesium/Widgets/widgets.css';
// Ours, and it must come after Cesium's: it drops the default body margin and
// gives #cesiumContainer the full viewport. Without it the globe renders
// short, with an 8px gutter around it.
import './style.css';
import * as Cesium from 'cesium';

import configData from './user-config.json';

import { WIND_API_URL } from './config.js';
import { WindField } from './windField.js';
import { WindVectorField } from './windVectors.js';
import { Tower } from './tower.js';
import {
  connectSimServer,
  fetchBalloonComms,
  setPaused,
  setHorizonCoeff,
  setBalloonCount,
  addTower,
  removeTower,
} from './simClient.js';
import { LinkLayer, parseNodeKey } from './linkLayer.js';
import { BalloonLayer } from './balloonLayer.js';
import { CommsReplay } from './commsReplay.js';
import { InspectorPanel } from './ui/inspectorPanel.js';
import { ControlPanel } from './ui/controlPanel.js';

// NOTE: treat this like any other API key — keep it out of version control,
// load it from an env var / untracked config file in a real project.
Cesium.Ion.defaultAccessToken = configData.CESIUM_ION_DEFAULT_ACCESS_TOKEN;

// ---------------------------------------------------------------------------
// Main
//
// Balloon/tower simulation state now lives in sim-server (Rust) — see
// sim-server/README.md. This file is a thin client: it renders whatever
// sim-server broadcasts over WebSocket and sends user actions (add/remove
// tower, change balloon count, change horizon coefficient) to it as REST
// commands. It owns no physics or link-detection logic of its own.
// ---------------------------------------------------------------------------
async function initCesium() {
  const terrainProvider = await Cesium.createWorldTerrainAsync();

  const viewer = new Cesium.Viewer('cesiumContainer', {
    terrainProvider: terrainProvider,
  });

  viewer.camera.flyTo({
    destination: Cesium.Cartesian3.fromDegrees(0, 20, 20000000), // zoomed out for global spawn
    duration: 0,
  });

  // --- Wind field ------------------------------------------------------------
  // Only used for the optional wind-vector-arrow visualization now — balloon
  // advection happens inside sim-server, which owns the authoritative wind
  // field. We fetch it from sim-server too (WIND_API_URL -> sim-server, not
  // wind_backend.py), so both share one source and can't disagree. This fetch
  // is NOT awaited here: the full /api/wind-levels payload is large and slow
  // (can take the better part of a minute), and there's no reason for it to
  // block the sim-server WebSocket connection or anything else below. It
  // resolves in the background; the wind-vectors panel controls just wait on
  // this promise themselves before they have anything to show.
  const windFieldPromise = WindField.fetchFromBackend(WIND_API_URL).catch((e) => {
    console.error('Failed to load wind field from backend, falling back to zero wind:', e);
    return new WindField(
      { nx: 1, ny: 1, lo1: -180, la1: 90, lo2: 180, la2: -90, dx: 360, dy: 180 },
      [{ pressureHpa: 500, altitudeM: 18000, u_data: [[0]], v_data: [[0]] }]
    );
  });

  // --- Wind vector field (arrows for a single selected pressure level) ---
  const windVectorField = new WindVectorField();

  // --- Towers and balloons: both now server-owned. These maps reconcile
  // Cesium entities against sim-server's snapshots (add/update/remove) --
  // no local physics or link-detection state lives here anymore.
  const balloonLayer = new BalloonLayer();
  const towerById = new Map(); // id -> Tower (rendering wrapper)

  // Towers are static once placed, so their Cartesian3 is derived on demand
  // rather than cached alongside the balloon positions.
  const towerPosition = (tower) =>
    Cesium.Cartesian3.fromDegrees(tower.lon, tower.lat, tower.heightM);
  const positionOfTower = (id) => {
    const tower = towerById.get(id);
    return tower ? towerPosition(tower) : undefined;
  };

  const commsReplay = new CommsReplay();

  const inspector = new InspectorPanel({
    onClose: () => deselectBalloon(),
    onReplay: () => {
      if (balloonLayer.selectedId !== null) fetchAndAnimateComms(balloonLayer.selectedId);
    },
  });

  async function fetchAndAnimateComms(id) {
    commsReplay.clear(viewer);
    inspector.clearComms();
    const comms = await fetchBalloonComms(id);
    if (!comms || id !== balloonLayer.selectedId) return; // selection moved on while fetching
    commsReplay.render(viewer, comms, (bid) => balloonLayer.positionOf(bid), positionOfTower);
    inspector.setComms(comms);
  }

  function selectBalloon(id) {
    balloonLayer.setSelected(id);
    inspector.show(id);
    fetchAndAnimateComms(id);
  }

  function deselectBalloon() {
    balloonLayer.setSelected(null);
    inspector.hide();
    commsReplay.clear(viewer);
  }

  function updateInspectorFromSnapshot(snapshot) {
    if (balloonLayer.selectedId === null) return;
    inspector.update(snapshot.balloons.find((x) => x.id === balloonLayer.selectedId) ?? null);
  }

  viewer.scene.morphComplete.addEventListener(() => balloonLayer.refreshRenderMode());

  function reconcileTowers(serverTowers) {
    const seen = new Set();
    for (const t of serverTowers) {
      seen.add(t.id);
      if (!towerById.has(t.id)) {
        const tower = new Tower(t.lon, t.lat, t.heightM);
        tower.addToScene(viewer);
        towerById.set(t.id, tower);
      }
      // Towers are static once created (no move command exists), so no
      // position update is needed on repeat sightings.
    }
    for (const [id, tower] of towerById) {
      if (!seen.has(id)) {
        tower.removeFromScene(viewer);
        towerById.delete(id);
      }
    }
  }

  // --- Radio link rendering -------------------------------------------------
  const linkLayer = new LinkLayer();

  // Resolves an edge endpoint ("b12" / "t3") to where that node is drawn right
  // now. Balloons move every tick; towers never do, so their position is
  // derived from the model on demand rather than cached.
  function resolveNodePosition(key) {
    const { kind, id } = parseNodeKey(key);
    if (kind === 'b') return balloonLayer.positionOf(id);
    return positionOfTower(id);
  }

  // --- Live control panel ----------------------------------------------------
  // Every control reports through a callback; the panel itself talks to
  // neither sim-server nor Cesium.
  const controlPanel = new ControlPanel({
    windFieldPromise,
    onPauseToggle: setPaused,
    onHorizonInput: () => {
      // Radio range depends on the coefficient, so every tower's gradient
      // overlay needs rebuilding to reflect the new value. Cheap enough at a
      // handful of towers; if you add many, consider debouncing.
      for (const tower of towerById.values()) tower.refreshRangeCircle(viewer);
    },
    onHorizonCommit: setHorizonCoeff,
    onBalloonCountCommit: setBalloonCount,
    onGlyphsChange: (on) => balloonLayer.setUseGlyphs(on),
    onOverlayChange: (mode) => balloonLayer.setOverlay(mode),
    onDeliveryMarksChange: (on) => balloonLayer.setDeliveryMarks(on),
    onWindChange: ({ enabled, levelIndex, stride }) => {
      if (enabled) {
        windFieldPromise.then((windField) =>
          windVectorField.render(viewer, windField, levelIndex, stride)
        );
      } else {
        windVectorField.clear(viewer);
      }
    },
  });
  // --- sim-server connection -------------------------------------------------
  // One authoritative snapshot stream; this client only renders it. Every
  // subsystem that reacts to a snapshot is fanned out from here.
  connectSimServer((snapshot) => {
    balloonLayer.reconcile(viewer, snapshot.balloons);
    reconcileTowers(snapshot.towers);
    if (snapshot.edges) {
      linkLayer.sync(viewer, snapshot.edges, resolveNodePosition);
    }
    linkLayer.refreshPositions(resolveNodePosition);
    controlPanel.syncFromSnapshot(snapshot, () => {
      for (const tower of towerById.values()) tower.refreshRangeCircle(viewer);
    });
    updateInspectorFromSnapshot(snapshot);
  });

  // --- User actions: add/remove tower, click on globe -----------------------
  const handler = new Cesium.ScreenSpaceEventHandler(viewer.scene.canvas);
  // Pick over a small rectangle rather than a single pixel, so clicking a
  // tower (a 12px point) or a balloon doesn't demand pixel-perfect aim. The
  // range-circle ellipses aren't tagged __isTower, so widening this can't
  // turn the whole circle into a delete target.
  const CLICK_PICK_TOLERANCE_PX = 12;
  // How many candidates to consider within that rectangle. A single nearest
  // hit (`scene.pick`) isn't enough here: a balloon that's actually in range
  // of a tower is, by definition, hovering near it, so its billboard very
  // often occupies the exact screen pixel closest to the click — at which
  // point `pick` returns the balloon and the tower is simply unreachable by
  // clicking, however wide the tolerance rectangle is. `drillPick` returns
  // everything hit in the rectangle so we can prefer a tower over a balloon
  // explicitly, rather than however the nearest-pixel search happens to
  // order them.
  const CLICK_DRILL_LIMIT = 8;
  const NEW_TOWER_HEIGHT_M = 30; // mast height for a tower dropped on the globe
  handler.setInputAction((click) => {
    const candidates = viewer.scene.drillPick(
      click.position,
      CLICK_DRILL_LIMIT,
      CLICK_PICK_TOLERANCE_PX,
      CLICK_PICK_TOLERANCE_PX
    );
    const pickedTower = candidates.find((c) => c.id && c.id.__isTower);
    if (pickedTower) {
      const entry = [...towerById.entries()].find(([, tower]) => tower.entity === pickedTower.id);
      if (entry) removeTower(entry[0]);
      return;
    }
    // No tower in range of the click — fall back to balloon selection.
    const pickedBalloon = candidates.find((c) => c.id && c.id.__balloonId !== undefined);
    if (pickedBalloon) {
      selectBalloon(pickedBalloon.id.__balloonId);
      return;
    }
    const cartesian = viewer.camera.pickEllipsoid(click.position, viewer.scene.globe.ellipsoid);
    if (!cartesian) return;
    const carto = Cesium.Cartographic.fromCartesian(cartesian);
    const lon = Cesium.Math.toDegrees(carto.longitude);
    const lat = Cesium.Math.toDegrees(carto.latitude);
    addTower(lon, lat, NEW_TOWER_HEIGHT_M);
  }, Cesium.ScreenSpaceEventType.LEFT_CLICK);

}

initCesium().catch((error) => {
  console.error('Error initializing Cesium:', error);
});
