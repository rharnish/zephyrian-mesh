import 'cesium/Build/Cesium/Widgets/widgets.css';
import * as Cesium from 'cesium';

import configData from './user-config.json';

import { params, WIND_API_URL } from './config.js';
import { WindField } from './windField.js';
import { WindVectorField } from './windVectors.js';
import { Tower } from './tower.js';
import {
  beliefKey,
  deliveryKey,
  bundleOutcome,
  commsAckLabel,
  BELIEF_CSS,
  BELIEF_LEGEND,
  BELIEF_VERDICT,
  DELIVERY_CSS,
  DELIVERY_LEGEND,
  COMMS_OUTCOME_CSS,
  MUTED_CSS,
} from './overlays.js';
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
import { BalloonLayer, OVERLAY_NONE, OVERLAY_BELIEF, OVERLAY_DELIVERY } from './balloonLayer.js';
import { CommsReplay } from './commsReplay.js';
import { InspectorPanel } from './ui/inspectorPanel.js';

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

  // Control-panel elements + drag/cooldown state, assigned once the panel
  // is built below. Lets other tabs' slider changes (arriving via
  // sim-server snapshots, since the server is the source of truth for both
  // values) update this tab's controls too, without fighting a slider the
  // user is actively dragging — or just released — in this tab right now.
  // The cooldown window matters because our own POST and the next
  // broadcast snapshot race: a snapshot computed just before the server
  // applied our change would otherwise snap the slider back to the old
  // value for a tick before jumping forward again, which reads as the
  // slider "resisting" quick successive changes.
  const REMOTE_SYNC_COOLDOWN_MS = 600;
  let horizonSlider, horizonValueLabel, numBalloonsSlider, numBalloonsValueLabel;
  let meshDegreeValue, meshDegreeBar, meshGroundedValue;
  let beliefLegend, beliefOkValue, beliefStaleValue, beliefUnawareValue, beliefNoneValue;

  // Mean degree at which a random geometric graph percolates. Below it the
  // mesh is islands, above it a giant component; see sim-server/src/bin/
  // mesh_depth.rs for the measurement this comes from.
  const PERCOLATION_DEGREE = 4.5;
  const MESH_DEGREE_BAR_MAX = 10; // full-width degree; threshold lands at 45%
  let isDraggingHorizon = false;
  let isDraggingBalloons = false;
  let horizonCooldownUntil = 0;
  let numBalloonsCooldownUntil = 0;

  // Pause state. The server is the single source of truth: `paused` and the
  // button label are updated ONLY from snapshot.paused (see
  // syncControlsFromSnapshot). A click/spacebar just POSTs the desired state
  // and waits for the server to echo it back. We deliberately do NOT flip
  // `paused` optimistically — doing so races the in-flight pre-change
  // snapshots and makes the toggle misfire (an earlier bug). One round-trip
  // of latency on the label is a fine price for a state that can't desync.
  let paused = false;
  let pauseToggle;
  // Inline SVG (fill:currentColor) rather than the ⏸/▶ unicode glyphs, which
  // render as orange emoji on most systems and ignore CSS `color`. currentColor
  // is set to white on the button, so these match the panel's other controls.
  const PAUSE_ICON =
    '<svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor" aria-hidden="true"><rect x="3" y="2" width="3" height="10"/><rect x="8" y="2" width="3" height="10"/></svg>';
  const PLAY_ICON =
    '<svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor" aria-hidden="true"><path d="M3 2 L12 7 L3 12 Z"/></svg>';
  function updatePauseButton() {
    if (!pauseToggle) return;
    // Media-player convention: show ⏸ while running (click to pause), ▶ while
    // paused (click to resume). Title carries the word for accessibility.
    pauseToggle.innerHTML = paused ? PLAY_ICON : PAUSE_ICON;
    pauseToggle.title = paused ? 'Resume' : 'Pause';
  }

  function syncControlsFromSnapshot(snapshot) {
    if (!horizonSlider) return; // panel not built yet
    const now = Date.now();
    if (typeof snapshot.paused === 'boolean' && snapshot.paused !== paused) {
      paused = snapshot.paused;
      updatePauseButton();
    }
    if (!isDraggingHorizon && now >= horizonCooldownUntil && typeof snapshot.horizonRefractionCoeff === 'number') {
      const coeff = snapshot.horizonRefractionCoeff;
      if (coeff !== params.horizonRefractionCoeff) {
        params.horizonRefractionCoeff = coeff;
        horizonSlider.value = coeff;
        horizonValueLabel.textContent = coeff.toFixed(2);
        for (const tower of towerById.values()) tower.refreshRangeCircle(viewer);
      }
    }
    if (!isDraggingBalloons && now >= numBalloonsCooldownUntil) {
      const n = snapshot.balloons.length;
      if (n !== params.numBalloons) {
        params.numBalloons = n;
        numBalloonsSlider.value = n;
        numBalloonsValueLabel.textContent = n;
      }
    }
    updateMeshHealth(snapshot);
  }

  // Unlike the sliders, this readout is never user-driven — it just mirrors
  // whatever the server last measured, so there's no drag/cooldown guard.
  function updateMeshHealth(snapshot) {
    if (!meshDegreeValue) return; // panel not built yet
    const degree = snapshot.meanDegree;
    const grounded = snapshot.groundedPct;
    if (typeof degree !== 'number' || typeof grounded !== 'number') return;

    meshDegreeValue.textContent = degree.toFixed(1);
    meshGroundedValue.textContent = `${grounded.toFixed(0)}%`;

    const pct = Math.min(100, (degree / MESH_DEGREE_BAR_MAX) * 100);
    meshDegreeBar.style.width = `${pct}%`;
    // Red well below the threshold, amber in the critical band either side of
    // it (where paths get long and delivery turns erratic), green above.
    const color =
      degree < PERCOLATION_DEGREE - 1 ? BELIEF_CSS.stale
      : degree < PERCOLATION_DEGREE + 1 ? BELIEF_CSS.unaware
      : BELIEF_CSS.ok;
    meshDegreeBar.style.backgroundColor = color;
    meshDegreeValue.style.color = color;

    if (typeof snapshot.believedGroundedPct !== 'number') return;
    // "believes and is right" is everything that believes, minus those whose
    // belief is stale; the remainder with no belief splits into unaware (a
    // route exists) and none.
    const stale = snapshot.beliefStalePct;
    const unaware = snapshot.beliefUnawarePct;
    const ok = snapshot.believedGroundedPct - stale;
    beliefOkValue.textContent = `${ok.toFixed(0)}%`;
    beliefStaleValue.textContent = `${stale.toFixed(0)}%`;
    beliefUnawareValue.textContent = `${unaware.toFixed(0)}%`;
    beliefNoneValue.textContent = `${Math.max(0, 100 - ok - stale - unaware).toFixed(0)}%`;
  }

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

  // --- sim-server connection -------------------------------------------------
  // One authoritative snapshot stream; this client only renders it. Every
  // subsystem that reacts to a snapshot is fanned out from here.
  connectSimServer((snapshot) => {
    balloonLayer.reconcile(viewer, snapshot.balloons);
    reconcileTowers(snapshot.towers);
    if (snapshot.edges) {
      linkLayer.sync(viewer, snapshot.edges);
    }
    linkLayer.refreshPositions(resolveNodePosition);
    syncControlsFromSnapshot(snapshot);
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

  // --- Live control panel ------------------------------------------------
  // Built directly in JS (no HTML file edits needed) — a simple fixed-
  // position overlay with two controls:
  //  - horizon coefficient: updates immediately, and rebuilds every
  //    tower's range-gradient overlay so it reflects the new value; synced
  //    to sim-server on release so radio range there matches the display.
  //  - balloon count: live label on drag, synced to sim-server on release
  //    as a respawn command (same pattern as the horizon coefficient).
  const panel = document.createElement('div');
  panel.style.cssText = `
    position: fixed; top: 10px; left: 10px; z-index: 1000;
    background: rgba(20, 20, 20, 0.75); color: #fff;
    font: 12px sans-serif; padding: 10px 12px; border-radius: 6px;
    display: flex; flex-direction: column; gap: 8px; width: 220px;
  `;
  panel.innerHTML = `
    <div style="display:flex; justify-content:space-between; align-items:center;">
      <span style="font-weight:bold;">Controls</span>
      <button id="panelCollapseToggle" title="Collapse" style="line-height:1;">&minus;</button>
    </div>
    <div id="panelBody" style="display:flex; flex-direction:column; gap:8px;">
      <div>
        <button id="pauseToggle" title="Pause"
                style="width:100%; padding:5px 0; cursor:pointer;
                       display:flex; align-items:center; justify-content:center;
                       background: rgba(0,0,0,0.25); color:#fff;
                       border:1px solid rgba(255,255,255,0.25); border-radius:4px;"></button>
        <div style="opacity:0.5; margin-top:3px; text-align:center;">(or press spacebar)</div>
      </div>
      <div style="border-top: 1px solid rgba(255,255,255,0.2); padding-top: 8px;">
        <label style="display:flex; justify-content:space-between;">
          <span>Horizon coeff.</span>
          <span id="horizonCoeffValue">${params.horizonRefractionCoeff.toFixed(2)}</span>
        </label>
        <input id="horizonCoeffSlider" type="range" min="2.5" max="4.2" step="0.05"
               value="${params.horizonRefractionCoeff}" style="width: 100%;" />
      </div>
      <div style="border-top: 1px solid rgba(255,255,255,0.2); padding-top: 8px;">
        <label style="display:flex; justify-content:space-between;">
          <span>Balloons</span>
          <span id="numBalloonsValue">${params.numBalloons}</span>
        </label>
        <input id="numBalloonsSlider" type="range" min="1" max="2000" step="10"
               value="${params.numBalloons}" style="width: 100%;" />
        <div style="display:flex; gap:6px; align-items:center; margin-top: 4px;">
          <input id="glyphsToggle" type="checkbox" />
          <label for="glyphsToggle" style="flex:1;">Glyphs</label>
        </div>
      </div>
      <!-- Mesh health. Both sliders above are really two ways of moving mean
           degree, and the mesh percolates around degree ~4.5 — below it the
           network shatters into islands, above it nearly everything reaches a
           tower. The tick mark is that threshold, so a drag shows how close
           the current settings are to falling apart. -->
      <div style="border-top: 1px solid rgba(255,255,255,0.2); padding-top: 8px;">
        <label style="display:flex; justify-content:space-between;">
          <span>Mesh degree</span>
          <span id="meshDegreeValue">&ndash;</span>
        </label>
        <div style="position:relative; height:5px; margin:5px 0 6px;
                    background:rgba(255,255,255,0.15); border-radius:3px;">
          <div id="meshDegreeBar"
               style="height:100%; width:0%; border-radius:3px; background:#888;
                      transition: width 0.2s linear, background-color 0.2s linear;"></div>
          <div title="percolation threshold (degree 4.5)"
               style="position:absolute; left:45%; top:-3px; bottom:-3px; width:1px;
                      background:rgba(255,255,255,0.8);"></div>
        </div>
        <label style="display:flex; justify-content:space-between;">
          <span>Grounded</span>
          <span id="meshGroundedValue">&ndash;</span>
        </label>
      </div>
      <!-- Belief vs. truth. Balloons only know what beacons told them, so
           their belief lags reality (stale) or trails behind it (unaware).
           See MESH_COMMS_DESIGN.md §1. -->
      <div style="border-top: 1px solid rgba(255,255,255,0.2); padding-top: 8px;">
        <div style="display:flex; gap:6px; align-items:center;">
          <input id="beliefOverlayToggle" type="checkbox" />
          <label for="beliefOverlayToggle" style="flex:1;">Belief overlay</label>
        </div>
        <div id="beliefLegend" style="display:none; margin-top:5px; opacity:0.85;">
          ${['ok', 'stale', 'unaware', 'none']
            .map(
              (key) => `
          <div style="display:flex; justify-content:space-between;">
            <span><span style="color:${BELIEF_CSS[key]};">&#9679;</span> ${BELIEF_LEGEND[key]}</span>
            <span id="belief${key[0].toUpperCase()}${key.slice(1)}Value">&ndash;</span>
          </div>`
            )
            .join('')}
        </div>
      </div>
      <!-- Last-delivery overlay. Server truth about how each balloon's most
           recent bundle actually got through — radio mesh vs. satellite
           release valve. See MESH_COMMS_DESIGN.md §3/§4. -->
      <div style="border-top: 1px solid rgba(255,255,255,0.2); padding-top: 8px;">
        <div style="display:flex; gap:6px; align-items:center;">
          <input id="deliveryOverlayToggle" type="checkbox" />
          <label for="deliveryOverlayToggle" style="flex:1;">Last-delivery overlay</label>
        </div>
        <div id="deliveryLegend" style="display:none; margin-top:5px; opacity:0.85;">
          ${['radio', 'satellite', 'none']
            .map(
              (key) => `
          <div style="display:flex; justify-content:space-between;">
            <span><span style="color:${DELIVERY_CSS[key]};">&#9679;</span> ${DELIVERY_LEGEND[key]}</span>
          </div>`
            )
            .join('')}
        </div>
      </div>
      <div style="display:flex; gap:6px; align-items:center; border-top: 1px solid rgba(255,255,255,0.2); padding-top: 8px;">
        <input id="windVectorsToggle" type="checkbox" />
        <label for="windVectorsToggle" style="flex:1;">Wind vectors</label>
      </div>
      <div>
        <select id="windVectorLevel" style="width: 100%;" disabled></select>
      </div>
      <div>
        <label style="display:flex; justify-content:space-between;">
          <span>Arrow density (stride)</span>
          <span id="windVectorStrideValue">12</span>
        </label>
        <input id="windVectorStride" type="range" min="2" max="12" step="1" value="12"
               style="width: 100%;" disabled />
      </div>
    </div>
  `;
  document.body.appendChild(panel);

  const panelBody = panel.querySelector('#panelBody');
  const panelCollapseToggle = panel.querySelector('#panelCollapseToggle');
  panelCollapseToggle.addEventListener('click', () => {
    const collapsed = panelBody.style.display === 'none';
    panelBody.style.display = collapsed ? 'flex' : 'none';
    panel.style.width = collapsed ? '220px' : 'auto';
    panelCollapseToggle.textContent = collapsed ? '−' : '+';
    panelCollapseToggle.title = collapsed ? 'Collapse' : 'Expand';
  });

  pauseToggle = panel.querySelector('#pauseToggle');
  updatePauseButton();
  pauseToggle.addEventListener('click', () => setPaused(!paused));

  // Spacebar toggles pause too — but not while typing in a form control.
  document.addEventListener('keydown', (e) => {
    if (e.code !== 'Space' && e.key !== ' ') return;
    const tag = document.activeElement && document.activeElement.tagName;
    if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA' || tag === 'BUTTON') return;
    e.preventDefault();
    setPaused(!paused);
  });

  const glyphsToggle = panel.querySelector('#glyphsToggle');
  glyphsToggle.addEventListener('change', () => {
    balloonLayer.setUseGlyphs(glyphsToggle.checked);
  });

  // Wind-vectors controls stay disabled until the (slow, backgrounded) wind
  // field fetch resolves — see windFieldPromise above.
  const levelSelect = panel.querySelector('#windVectorLevel');
  const windVectorsToggle = panel.querySelector('#windVectorsToggle');
  const strideSlider = panel.querySelector('#windVectorStride');
  const strideValueLabel = panel.querySelector('#windVectorStrideValue');
  windVectorsToggle.disabled = true;
  levelSelect.innerHTML = '<option>Loading wind data...</option>';

  let windField = null;
  function refreshWindVectors() {
    if (!windField) return;
    if (windVectorsToggle.checked) {
      windVectorField.render(viewer, windField, parseInt(levelSelect.value, 10), parseInt(strideSlider.value, 10));
    } else {
      windVectorField.clear(viewer);
    }
  }
  windFieldPromise.then((resolvedWindField) => {
    windField = resolvedWindField;
    levelSelect.innerHTML = '';
    windField.levels.forEach((level, idx) => {
      const opt = document.createElement('option');
      opt.value = idx;
      opt.textContent = `${level.pressureHpa} hPa (~${(level.altitudeM / 1000).toFixed(1)} km)`;
      levelSelect.appendChild(opt);
    });
    windVectorsToggle.disabled = false;
  });

  windVectorsToggle.addEventListener('change', () => {
    levelSelect.disabled = !windVectorsToggle.checked;
    strideSlider.disabled = !windVectorsToggle.checked;
    refreshWindVectors();
  });
  levelSelect.addEventListener('change', refreshWindVectors);
  strideSlider.addEventListener('change', () => {
    // 'change' (on release), not 'input' (every drag tick) — rebuilding the
    // whole arrow field is too expensive to do on every pixel of slider drag.
    strideValueLabel.textContent = strideSlider.value;
    refreshWindVectors();
  });
  strideSlider.addEventListener('input', () => {
    strideValueLabel.textContent = strideSlider.value; // live label, cheap
  });

  horizonSlider = panel.querySelector('#horizonCoeffSlider');
  horizonValueLabel = panel.querySelector('#horizonCoeffValue');
  horizonSlider.addEventListener('input', () => {
    isDraggingHorizon = true;
    params.horizonRefractionCoeff = parseFloat(horizonSlider.value);
    horizonValueLabel.textContent = params.horizonRefractionCoeff.toFixed(2);
    // Range depends on the coefficient, so every tower's gradient overlay
    // needs rebuilding to reflect the new value. Cheap enough to do on
    // every slider tick at a handful of towers; if you add many towers,
    // consider debouncing this.
    for (const tower of towerById.values()) tower.refreshRangeCircle(viewer);
  });
  horizonSlider.addEventListener('change', () => {
    // Sync to sim-server only on release (not every drag tick) — this
    // triggers a full grid rebuild there, so it's not something to send on
    // every 'input' event. Server echoes it back in every snapshot, which
    // is how other tabs pick up the change (see syncControlsFromSnapshot).
    isDraggingHorizon = false;
    horizonCooldownUntil = Date.now() + REMOTE_SYNC_COOLDOWN_MS;
    setHorizonCoeff(params.horizonRefractionCoeff);
  });

  meshDegreeValue = panel.querySelector('#meshDegreeValue');
  meshDegreeBar = panel.querySelector('#meshDegreeBar');
  meshGroundedValue = panel.querySelector('#meshGroundedValue');

  beliefLegend = panel.querySelector('#beliefLegend');
  beliefOkValue = panel.querySelector('#beliefOkValue');
  beliefStaleValue = panel.querySelector('#beliefStaleValue');
  beliefUnawareValue = panel.querySelector('#beliefUnawareValue');
  beliefNoneValue = panel.querySelector('#beliefNoneValue');
  const beliefOverlayToggle = panel.querySelector('#beliefOverlayToggle');
  const deliveryLegend = panel.querySelector('#deliveryLegend');
  const deliveryOverlayToggle = panel.querySelector('#deliveryOverlayToggle');
  // The two overlays are mutually exclusive — both recolor every balloon, and
  // showing two at once would just make each one illegible. Driving a single
  // mode off whichever box was ticked enforces that by construction, rather
  // than each handler remembering to un-tick the other.
  function applyOverlaySelection(mode) {
    balloonLayer.setOverlay(mode);
    beliefOverlayToggle.checked = mode === OVERLAY_BELIEF;
    deliveryOverlayToggle.checked = mode === OVERLAY_DELIVERY;
    beliefLegend.style.display = mode === OVERLAY_BELIEF ? 'block' : 'none';
    deliveryLegend.style.display = mode === OVERLAY_DELIVERY ? 'block' : 'none';
  }
  beliefOverlayToggle.addEventListener('change', () =>
    applyOverlaySelection(beliefOverlayToggle.checked ? OVERLAY_BELIEF : OVERLAY_NONE)
  );
  deliveryOverlayToggle.addEventListener('change', () =>
    applyOverlaySelection(deliveryOverlayToggle.checked ? OVERLAY_DELIVERY : OVERLAY_NONE)
  );

  numBalloonsSlider = panel.querySelector('#numBalloonsSlider');
  numBalloonsValueLabel = panel.querySelector('#numBalloonsValue');
  numBalloonsSlider.addEventListener('input', () => {
    isDraggingBalloons = true;
    numBalloonsValueLabel.textContent = numBalloonsSlider.value; // live label, cheap
  });
  numBalloonsSlider.addEventListener('change', () => {
    // Sync to sim-server only on release (not every drag tick) — this
    // triggers a full clear+respawn of every balloon there, so it's not
    // something to send on every 'input' event. Server echoes the live
    // visible count in every snapshot, which is how other tabs pick up
    // the change (see syncControlsFromSnapshot).
    isDraggingBalloons = false;
    numBalloonsCooldownUntil = Date.now() + REMOTE_SYNC_COOLDOWN_MS;
    const requested = parseInt(numBalloonsSlider.value, 10);
    params.numBalloons = requested;
    setBalloonCount(requested);
  });
}

initCesium().catch((error) => {
  console.error('Error initializing Cesium:', error);
});
