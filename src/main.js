import 'cesium/Build/Cesium/Widgets/widgets.css';
import * as Cesium from 'cesium';

import configData from './user-config.json';

import { params, WIND_API_URL, SIM_SERVER_URL, SIM_SERVER_WS_URL } from './config.js';
import { WindField } from './windField.js';
import { WindVectorField } from './windVectors.js';
import { Tower } from './tower.js';

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
  // advection happens inside sim-server, using its own independently-fetched
  // copy of the same data. This fetch is NOT awaited here: the full
  // /api/wind-levels payload is large and slow (can take the better part of
  // a minute), and there's no reason for it to block the sim-server
  // WebSocket connection or anything else below. It resolves in the
  // background; the wind-vectors panel controls just wait on this promise
  // themselves before they have anything to show.
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
  const balloonEntities = new Map(); // id -> Cesium.Entity
  const towerById = new Map(); // id -> Tower (rendering wrapper)

  function reconcileBalloons(serverBalloons) {
    const seen = new Set();
    for (const b of serverBalloons) {
      seen.add(b.id);
      const position = Cesium.Cartesian3.fromDegrees(b.lon, b.lat, b.alt);
      const entity = balloonEntities.get(b.id);
      if (entity) {
        entity.position = position;
      } else {
        const newEntity = viewer.entities.add({
          position,
          point: { pixelSize: 6, color: Cesium.Color.CYAN },
        });
        balloonEntities.set(b.id, newEntity);
      }
    }
    for (const [id, entity] of balloonEntities) {
      if (!seen.has(id)) {
        viewer.entities.remove(entity);
        balloonEntities.delete(id);
      }
    }
  }

  function reconcileTowers(serverTowers) {
    const seen = new Set();
    for (const t of serverTowers) {
      seen.add(t.id);
      if (!towerById.has(t.id)) {
        const tower = new Tower(t.lon, t.lat, t.heightM);
        tower.addToScene(viewer, `T-${t.id}`);
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
  const linkCollection = new Cesium.PolylineCollection();
  viewer.scene.primitives.add(linkCollection);

  const GROUNDED_LINK_COLOR = Cesium.Color.LIME.withAlpha(0.6);   // cluster reaches a tower
  const UNGROUNDED_LINK_COLOR = Cesium.Color.GRAY.withAlpha(0.5); // balloon-only cluster

  // pairKey -> live Polyline primitive, so unchanged links are reused
  // instead of destroyed/recreated every tick.
  const linkPrimitives = new Map();

  function syncLinks(edges) {
    const edgesByPairKey = new Map(edges.map((e) => [e.pairKey, e]));

    // Remove links that no longer exist.
    for (const [pairKey, primitive] of linkPrimitives) {
      if (!edgesByPairKey.has(pairKey)) {
        linkCollection.remove(primitive);
        linkPrimitives.delete(pairKey);
      }
    }
    // Add or update current links.
    for (const edge of edges) {
      const posA = Cesium.Cartesian3.fromDegrees(edge.a[0], edge.a[1], edge.a[2]);
      const posB = Cesium.Cartesian3.fromDegrees(edge.b[0], edge.b[1], edge.b[2]);
      const color = edge.grounded ? GROUNDED_LINK_COLOR : UNGROUNDED_LINK_COLOR;
      const existing = linkPrimitives.get(edge.pairKey);
      if (existing) {
        existing.positions = [posA, posB];
        existing.material.uniforms.color = color;
      } else {
        const primitive = linkCollection.add({
          positions: [posA, posB],
          width: 2,
          material: Cesium.Material.fromType('Color', { color }),
        });
        linkPrimitives.set(edge.pairKey, primitive);
      }
    }
  }

  // --- sim-server connection -------------------------------------------------
  // One authoritative snapshot stream; this client only renders it.
  function connectSimServer() {
    const ws = new WebSocket(SIM_SERVER_WS_URL);
    ws.onmessage = (event) => {
      const snapshot = JSON.parse(event.data);
      reconcileBalloons(snapshot.balloons);
      reconcileTowers(snapshot.towers);
      if (snapshot.edges) {
        syncLinks(snapshot.edges);
      }
    };
    ws.onerror = (e) => console.error('sim-server WebSocket error (is sim-server running?):', e);
    ws.onclose = () => {
      console.warn('sim-server WebSocket closed — retrying in 2s');
      setTimeout(connectSimServer, 2000);
    };
  }
  connectSimServer();

  // --- User actions: add/remove tower, click on globe -----------------------
  const handler = new Cesium.ScreenSpaceEventHandler(viewer.scene.canvas);
  handler.setInputAction((click) => {
    const picked = viewer.scene.pick(click.position);
    if (Cesium.defined(picked) && picked.id && picked.id.__isTower) {
      const entry = [...towerById.entries()].find(([, tower]) => tower.entity === picked.id);
      if (entry) {
        const [id] = entry;
        fetch(`${SIM_SERVER_URL}/api/towers/${id}`, { method: 'DELETE' }).catch((e) =>
          console.error('Failed to remove tower:', e)
        );
      }
      return;
    }
    const cartesian = viewer.camera.pickEllipsoid(click.position, viewer.scene.globe.ellipsoid);
    if (!cartesian) return;
    const carto = Cesium.Cartographic.fromCartesian(cartesian);
    const lon = Cesium.Math.toDegrees(carto.longitude);
    const lat = Cesium.Math.toDegrees(carto.latitude);
    fetch(`${SIM_SERVER_URL}/api/towers`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ lon, lat, heightM: 30 }),
    }).catch((e) => console.error('Failed to add tower:', e));
  }, Cesium.ScreenSpaceEventType.LEFT_CLICK);

  // --- Live control panel ------------------------------------------------
  // Built directly in JS (no HTML file edits needed) — a simple fixed-
  // position overlay with two controls:
  //  - horizon coefficient: updates immediately, and rebuilds every
  //    tower's range-gradient overlay so it reflects the new value; synced
  //    to sim-server on release so radio range there matches the display.
  //  - balloon count: applied on click (not live-as-you-type), sent to
  //    sim-server as a respawn command.
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
        <label style="display:flex; justify-content:space-between;">
          <span>Horizon coeff.</span>
          <span id="horizonCoeffValue">${params.horizonRefractionCoeff.toFixed(2)}</span>
        </label>
        <input id="horizonCoeffSlider" type="range" min="2" max="6" step="0.05"
               value="${params.horizonRefractionCoeff}" style="width: 100%;" />
      </div>
      <div style="display:flex; gap:6px; align-items:center;">
        <label style="flex:1;">Balloons</label>
        <input id="numBalloonsInput" type="number" min="1" max="5000" step="10"
               value="${params.numBalloons}" style="width: 70px;" />
        <button id="applyNumBalloons">Apply</button>
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
        <input id="windVectorStride" type="range" min="2" max="30" step="1" value="12"
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

  const horizonSlider = panel.querySelector('#horizonCoeffSlider');
  const horizonValueLabel = panel.querySelector('#horizonCoeffValue');
  horizonSlider.addEventListener('input', () => {
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
    // every 'input' event.
    fetch(`${SIM_SERVER_URL}/api/horizon-coeff`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ coeff: params.horizonRefractionCoeff }),
    }).catch((e) => console.error('Failed to sync horizon coefficient to sim-server:', e));
  });

  const numBalloonsInput = panel.querySelector('#numBalloonsInput');
  const applyNumBalloonsBtn = panel.querySelector('#applyNumBalloons');
  applyNumBalloonsBtn.addEventListener('click', () => {
    const requested = parseInt(numBalloonsInput.value, 10);
    if (!Number.isFinite(requested) || requested < 1) return;
    params.numBalloons = requested;
    fetch(`${SIM_SERVER_URL}/api/balloons/count`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ n: requested }),
    }).catch((e) => console.error('Failed to set balloon count on sim-server:', e));
  });
}

initCesium().catch((error) => {
  console.error('Error initializing Cesium:', error);
});
