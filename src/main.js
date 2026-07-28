import 'cesium/Build/Cesium/Widgets/widgets.css';
import * as Cesium from 'cesium';

import configData from './user-config.json';

import { params, WIND_API_URL } from './config.js';
import { WindField } from './windField.js';
import { WindVectorField } from './windVectors.js';
import { Tower } from './tower.js';
import { balloonIconForAltitude, BALLOON_ICON_WIDTH, BALLOON_ICON_HEIGHT } from './balloonIcon.js';
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
  const balloonEntities = new Map(); // id -> Cesium.Entity
  const balloonPositions = new Map(); // id -> Cesium.Cartesian3, mirrors balloonEntities for cheap link-line lookups
  const towerById = new Map(); // id -> Tower (rendering wrapper)

  // Towers are static once placed, so their Cartesian3 is derived on demand
  // rather than cached alongside the balloon positions.
  const towerPosition = (tower) =>
    Cesium.Cartesian3.fromDegrees(tower.lon, tower.lat, tower.heightM);

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

  const BALLOON_COLOR = Cesium.Color.fromCssColorString('#d9dbe0');
  const SELECTED_BALLOON_COLOR = Cesium.Color.fromCssColorString('#3fd0ff');

  // The overlay palettes live in overlays.js as CSS strings, because the
  // panels below render them as HTML and the entities here need them as
  // Cesium colors. Converting once, in one place, is what stops the two
  // representations drifting apart.
  const toCesiumColors = (cssTable) =>
    Object.fromEntries(
      Object.entries(cssTable).map(([key, css]) => [key, Cesium.Color.fromCssColorString(css)])
    );
  const BELIEF_COLORS = toCesiumColors(BELIEF_CSS);
  const DELIVERY_COLORS = toCesiumColors(DELIVERY_CSS);
  const COMMS_OUTCOME_COLORS = toCesiumColors(COMMS_OUTCOME_CSS);

  let beliefOverlayEnabled = false;
  let deliveryOverlayEnabled = false;

  // Animated packet along the recorded path (MESH_COMMS_DESIGN.md §3/C4).
  // On selection, fetch the balloon's most recently *resolved* bundle from
  // the sim-server (GET /api/balloons/:id/comms — the first query endpoint;
  // everything else is fire-and-forget) and replay it: a dot travels the
  // bundle's actual recorded path (not a recomputed shortest path), then the
  // ack's fate plays out — all the way back if acked, partway if it died en
  // route, or not at all if the bundle went out via satellite or never
  // reached a tower. This is a replay of the last resolved bundle, not a
  // live view of one currently in flight — a bundle can take many rounds per
  // hop, so watching one "live" would mostly look idle.
  const COMMS_PATH_COLOR = Cesium.Color.fromCssColorString('#ffd166');
  const COMMS_HOP_DURATION_MS = 550;

  const commsPathCollection = new Cesium.PolylineCollection();
  viewer.scene.primitives.add(commsPathCollection);
  let commsPathPrimitive = null;
  let commsPacketEntity = null;
  let commsAnimationFrame = null;
  // Bumped on every clear so an in-flight animation's frame callback can tell
  // it's been superseded (new selection, or the same one re-fetched) and stop
  // touching an entity that may already be gone.
  let commsAnimationToken = 0;
  let selectedComms = null; // last-fetched GET /api/balloons/:id/comms response

  function clearCommsAnimation() {
    commsAnimationToken++;
    if (commsAnimationFrame !== null) {
      cancelAnimationFrame(commsAnimationFrame);
      commsAnimationFrame = null;
    }
    if (commsPathPrimitive) {
      commsPathCollection.remove(commsPathPrimitive);
      commsPathPrimitive = null;
    }
    if (commsPacketEntity) {
      viewer.entities.remove(commsPacketEntity);
      commsPacketEntity = null;
    }
  }

  // Animates a dot across a sequence of positions, one hop per
  // COMMS_HOP_DURATION_MS, then calls `onDone`. Reuses `commsPacketEntity`
  // across legs (outbound, then the ack's reverse leg) so the dot doesn't
  // jump between them.
  function animateCommsPacket(positions, color, onDone) {
    const token = commsAnimationToken;
    if (!commsPacketEntity) {
      commsPacketEntity = viewer.entities.add({
        position: positions[0],
        point: { pixelSize: 10, color, outlineColor: Cesium.Color.BLACK, outlineWidth: 1 },
      });
    } else {
      commsPacketEntity.point.color = color;
      commsPacketEntity.position = positions[0];
    }
    if (positions.length < 2) {
      onDone();
      return;
    }
    let hop = 0;
    const totalHops = positions.length - 1;
    let hopStart = performance.now();
    function frame(now) {
      if (token !== commsAnimationToken) return; // superseded — stop touching this entity
      const t = Math.min(1, (now - hopStart) / COMMS_HOP_DURATION_MS);
      commsPacketEntity.position = Cesium.Cartesian3.lerp(
        positions[hop], positions[hop + 1], t, new Cesium.Cartesian3()
      );
      if (t >= 1) {
        hop++;
        if (hop >= totalHops) {
          onDone();
          return;
        }
        hopStart = now;
      }
      commsAnimationFrame = requestAnimationFrame(frame);
    }
    commsAnimationFrame = requestAnimationFrame(frame);
  }

  // Renders (and animates) the selected balloon's last resolved bundle.
  // Positions are snapshotted once at render time — a deliberate replay of a
  // *past* path using each balloon's *current* position, same "possibly
  // stale" idiom the rest of this design leans on rather than a hard error.
  function renderCommsAnimation(comms) {
    const lb = comms && comms.lastBundle;
    if (!lb || !lb.path) return; // still Pending, or nothing originated yet

    const positions = lb.path.map((id) => balloonPositions.get(id)).filter(Boolean);
    if (positions.length !== lb.path.length) return; // a hop balloon isn't currently visible

    // The recorded path only ever holds balloon ids — delivery to a tower is
    // modeled as instantaneous from the last tower-adjacent balloon, so the
    // tower itself is never a hop. Append its position so the drawn/animated
    // path actually reaches the tower instead of stopping one hop short.
    let towerIncluded = false;
    if (lb.towerId !== null && lb.towerId !== undefined) {
      const tower = towerById.get(lb.towerId);
      if (tower) {
        positions.push(towerPosition(tower));
        towerIncluded = true;
      }
    }
    // Ack hops are counted purely over balloon-to-balloon hops (see
    // `Ack.total_hops` in bundle.rs) — the tower hand-off above was never a
    // modeled ack hop. The reverse path always starts at the tower though
    // (that's where the ack is created), so the partial-reverse slice always
    // shows that leg plus however many balloon hops the ack actually made.
    const balloonHops = lb.path.length - 1;
    const towerLeg = towerIncluded ? 1 : 0;

    commsPathPrimitive = commsPathCollection.add({
      positions,
      width: 3,
      material: Cesium.Material.fromType('PolylineDash', { color: COMMS_PATH_COLOR, dashLength: 12 }),
    });

    const outcome = bundleOutcome(lb);
    const outcomeColor = COMMS_OUTCOME_COLORS[outcome];

    animateCommsPacket(positions, COMMS_PATH_COLOR, () => {
      // Satellite pickup and a mesh drop both end the story where the
      // outbound leg stopped — there is no ack to animate, only a recolor.
      if (outcome === 'satellite' || outcome === 'droppedInMesh') {
        commsPacketEntity.point.color = outcomeColor;
        return;
      }
      // The ack retraces the path. A completed one runs the whole way home;
      // one that died is truncated to however far it actually got.
      const reverse = [...positions].reverse();
      const ackPath =
        outcome === 'acked'
          ? reverse
          : reverse.slice(0, Math.max(0, Math.min(balloonHops, lb.ackHopsCompleted)) + towerLeg + 1);
      animateCommsPacket(ackPath, outcomeColor, () => {
        commsPacketEntity.point.color = outcomeColor;
      });
    });
  }

  // Comms log panel (MESH_COMMS_DESIGN.md §3): a row per retained telemetry
  // record for the selected balloon, newest first — `seq · round · hops ·
  // channel · ack state`, plus hash-prefix/tamper columns kept as explicit
  // placeholders (C3's hash chain hasn't landed yet, so there's nothing
  // honest to show there beyond "—").
  let commsLogPanel, commsLogTableBody; // assigned when the panel is built

  function commsLogRowHtml(record) {
    // A record's channel is null until its bundle resolves, so anything that
    // isn't one of the two real channels reads as "—", not as a state.
    // Note this column is tinted from the *outcome* palette, not the delivery
    // one — radio reads as the acked green rather than the overlay's lime.
    const resolved = record.channel === 'radio' || record.channel === 'satellite';
    const channelText = resolved ? record.channel : '—';
    const channelColor =
      record.channel === 'radio' ? COMMS_OUTCOME_CSS.acked
      : record.channel === 'satellite' ? COMMS_OUTCOME_CSS.satellite
      : MUTED_CSS;
    const ack = commsAckLabel(record.ackState, record.ackHopsCompleted, record.hops);
    return `
      <tr>
        <td>${record.createdAtRound}</td>
        <td>${record.seq}</td>
        <td>${record.hops ?? '—'}</td>
        <td style="color:${channelColor};">${channelText}</td>
        <td style="color:${ack.css};">${ack.text}</td>
        <td style="opacity:0.5;" title="Needs C3's hash chain">&mdash;</td>
        <td style="opacity:0.5;" title="Needs C3's hash chain">&mdash;</td>
      </tr>
    `;
  }

  function renderCommsLogPanel(comms) {
    if (!commsLogPanel || !commsLogTableBody) return;
    const log = comms && comms.log;
    if (!log || log.length === 0) {
      commsLogPanel.style.display = 'none';
      return;
    }
    commsLogPanel.style.display = 'block';
    commsLogTableBody.innerHTML = log.map(commsLogRowHtml).join('');
  }

  function clearCommsLogPanel() {
    if (commsLogPanel) commsLogPanel.style.display = 'none';
  }

  async function fetchAndAnimateComms(id) {
    clearCommsAnimation();
    clearCommsLogPanel();
    selectedComms = null;
    const comms = await fetchBalloonComms(id);
    if (!comms || id !== selectedBalloonId) return; // selection moved on while fetching
    selectedComms = comms;
    renderCommsAnimation(selectedComms);
    renderCommsLogPanel(selectedComms);
  }

  // Balloon selection/inspection. Clicking a balloon selects it; the inspector
  // panel (built below) shows its live position/altitude. This is the surface
  // the richer measurements + comms/tamper details attach to later — see
  // MESH_COMMS_DESIGN.md.
  let selectedBalloonId = null;
  let inspectorPanel, inspectorBody, inspectorTitle; // assigned when the panel is built

  // Single place that decides a balloon's tint: selection wins, then the
  // belief overlay if enabled, then the default. Called on selection change,
  // on overlay toggle, and whenever a balloon's belief state changes.
  function applyBalloonColor(id) {
    const e = balloonEntities.get(id);
    if (!e) return;
    const selected = id === selectedBalloonId;
    const color = selected
      ? SELECTED_BALLOON_COLOR
      : deliveryOverlayEnabled
        ? DELIVERY_COLORS[e.__deliveryKey] ?? BALLOON_COLOR
        : beliefOverlayEnabled
          ? BELIEF_COLORS[e.__beliefKey] ?? BALLOON_COLOR
          : BALLOON_COLOR;
    e.point.color = color;
    e.point.pixelSize = selected ? 11 : 6;
    e.billboard.color = color;
  }

  function selectBalloon(id) {
    const previous = selectedBalloonId;
    // Update the selection *before* recoloring, since applyBalloonColor reads
    // selectedBalloonId to decide the tint.
    selectedBalloonId = id;
    if (previous !== null && previous !== id) applyBalloonColor(previous);
    applyBalloonColor(id);
    if (inspectorPanel) inspectorPanel.style.display = 'block';
    if (inspectorTitle) inspectorTitle.textContent = `Balloon #${id}`;
    fetchAndAnimateComms(id);
  }

  function deselectBalloon() {
    const previous = selectedBalloonId;
    selectedBalloonId = null;
    if (previous !== null) applyBalloonColor(previous);
    if (inspectorPanel) inspectorPanel.style.display = 'none';
    clearCommsAnimation();
    clearCommsLogPanel();
    selectedComms = null;
  }

  // "82.32 W", "29.65 N" — same convention as the tower labels (towerModel.js).
  const fmtLon = (lon) => `${Math.abs(lon).toFixed(3)}° ${lon < 0 ? 'W' : 'E'}`;
  const fmtLat = (lat) => `${Math.abs(lat).toFixed(3)}° ${lat < 0 ? 'S' : 'N'}`;

  // Text summary of the last-bundle animation, matching its colors. Reads
  // from `selectedComms` (fetched once on selection), not the per-snapshot
  // balloon — so this stays stable across the ~50ms snapshot cadence that
  // rebuilds the rest of the inspector.
  function commsSummaryHtml() {
    if (selectedComms === undefined || selectedComms === null) {
      return `<div style="opacity:0.6;">Loading&hellip;</div>`;
    }
    const lb = selectedComms.lastBundle;
    if (!lb) return `<div style="opacity:0.6;">No bundle originated yet.</div>`;
    if (!lb.path) return `<div style="opacity:0.6;">Pending &mdash; not resolved yet.</div>`;

    // Same classification the animation runs on, so the dot and this text can
    // never tell two different stories about one bundle.
    const total = lb.path.length - 1;
    const outcome = bundleOutcome(lb);
    const outcomeText = {
      satellite: 'picked up by satellite',
      acked: 'delivered and acked',
      ackDied: `delivered, ack died after ${lb.ackHopsCompleted}/${total} hop${total === 1 ? '' : 's'}`,
      droppedInMesh: 'dropped in the mesh (loop/TTL) — never reached a tower',
    }[outcome];
    return `
      <div style="display:flex; justify-content:space-between;"><span>Seq</span><span>${lb.seq}</span></div>
      <div style="display:flex; justify-content:space-between;"><span>Hops</span><span>${total}</span></div>
      <div style="display:flex; justify-content:space-between;"><span>Outcome</span><span style="color:${COMMS_OUTCOME_CSS[outcome]};">${outcomeText}</span></div>
    `;
  }

  function updateInspectorFromSnapshot(snapshot) {
    if (selectedBalloonId === null || !inspectorBody) return;
    const b = snapshot.balloons.find((x) => x.id === selectedBalloonId);
    if (!b) {
      inspectorBody.innerHTML =
        `<div style="opacity:0.7;">Not in the active set right now (raise the balloon count to bring it back).</div>`;
      return;
    }
    // Deliberately shows the balloon's belief and the truth as two separate
    // rows: the balloon acts on the former and has no access to the latter.
    const key = beliefKey(b);
    const beliefText =
      b.believedHops === null || b.believedHops === undefined
        ? 'no route known'
        : `${b.believedHops} hop${b.believedHops === 1 ? '' : 's'} to a tower`;
    const dKey = deliveryKey(b);
    const channelText = DELIVERY_LEGEND[dKey];
    const channelColor = DELIVERY_CSS[dKey] ?? MUTED_CSS;
    inspectorBody.innerHTML = `
      <div style="display:flex; justify-content:space-between;"><span>Latitude</span><span>${fmtLat(b.lat)}</span></div>
      <div style="display:flex; justify-content:space-between;"><span>Longitude</span><span>${fmtLon(b.lon)}</span></div>
      <div style="display:flex; justify-content:space-between;"><span>Altitude</span><span>${(b.alt / 1000).toFixed(2)} km</span></div>
      <div style="display:flex; justify-content:space-between; margin-top:6px;"><span>Believes</span><span>${beliefText}</span></div>
      <div style="display:flex; justify-content:space-between;"><span>Actually grounded</span><span>${b.grounded ? 'yes' : 'no'}</span></div>
      <div style="display:flex; justify-content:space-between;"><span>Verdict</span><span style="color:${BELIEF_CSS[key]};">${BELIEF_VERDICT[key]}</span></div>
      <div style="display:flex; justify-content:space-between;"><span>Last delivered via</span><span style="color:${channelColor};">${channelText}</span></div>
      <div style="border-top: 1px solid rgba(255,255,255,0.2); margin-top:8px; padding-top:6px;">
        <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:2px;">
          <span style="font-weight:bold;">Last bundle</span>
          <button id="commsReplayBtn" title="Replay the animation" style="font-size:11px; padding:1px 6px; cursor:pointer;">&#8635; Replay</button>
        </div>
        ${commsSummaryHtml()}
      </div>
      <div style="margin-top:6px; opacity:0.55; font-style:italic; line-height:1.4;">
        Measurements (gas, ballast, temperature, humidity) and the message log /
        tamper chain will appear here once those systems are built — see
        MESH_COMMS_DESIGN.md.
      </div>
    `;
  }

  // Dots are the default; the "Glyphs" checkbox below opts into the
  // altitude-glyph billboard. The checkbox is authoritative in *every* scene
  // mode, 2D included — glyph detail does read as noise at flat-map zoom
  // levels, but that's a judgement for whoever is looking, not something to
  // enforce by overriding the control.
  let useGlyphs = false;
  function useDots() {
    return !useGlyphs;
  }

  function reconcileBalloons(serverBalloons) {
    const show2D = useDots();
    const seen = new Set();
    for (const b of serverBalloons) {
      seen.add(b.id);
      const position = Cesium.Cartesian3.fromDegrees(b.lon, b.lat, b.alt);
      balloonPositions.set(b.id, position);
      const icon = balloonIconForAltitude(b.alt);
      const entity = balloonEntities.get(b.id);
      const key = beliefKey(b);
      const dKey = deliveryKey(b);
      if (entity) {
        entity.position = position;
        entity.billboard.image = icon;
        // Only touch color when the relevant overlay state actually changed —
        // this runs for every balloon every snapshot.
        if (entity.__beliefKey !== key) {
          entity.__beliefKey = key;
          if (beliefOverlayEnabled) applyBalloonColor(b.id);
        }
        if (entity.__deliveryKey !== dKey) {
          entity.__deliveryKey = dKey;
          if (deliveryOverlayEnabled) applyBalloonColor(b.id);
        }
      } else {
        const newEntity = viewer.entities.add({
          position,
          point: {
            pixelSize: 6,
            color: BALLOON_COLOR,
            show: show2D,
          },
          billboard: {
            image: icon,
            width: BALLOON_ICON_WIDTH,
            height: BALLOON_ICON_HEIGHT,
            color: BALLOON_COLOR,
            verticalOrigin: Cesium.VerticalOrigin.BOTTOM,
            show: !show2D,
          },
        });
        newEntity.__balloonId = b.id; // lets click-picking map back to a balloon id
        newEntity.__beliefKey = key;
        newEntity.__deliveryKey = dKey;
        balloonEntities.set(b.id, newEntity);
        if (b.id === selectedBalloonId || beliefOverlayEnabled || deliveryOverlayEnabled) applyBalloonColor(b.id);
      }
    }
    for (const [id, entity] of balloonEntities) {
      if (!seen.has(id)) {
        viewer.entities.remove(entity);
        balloonEntities.delete(id);
        balloonPositions.delete(id);
      }
    }
  }

  // Flip every existing balloon between dot and glyph rendering when the
  // "Glyphs" checkbox changes (see the call site further below).
  //
  // Also re-asserted on morphComplete. Strictly that's redundant now that
  // rendering no longer depends on scene mode — entity graphics keep their
  // `show` values across a morph — but it's one call per morph and there is
  // an open, unreproduced report of entity/primitive state desyncing across
  // exactly this event, so it stays as belt-and-braces.
  function updateBalloonRenderModeForAll() {
    const show2D = useDots();
    for (const entity of balloonEntities.values()) {
      entity.point.show = show2D;
      entity.billboard.show = !show2D;
    }
  }
  viewer.scene.morphComplete.addEventListener(updateBalloonRenderModeForAll);

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
    if (kind === 'b') return balloonPositions.get(id);
    const tower = towerById.get(id);
    return tower ? towerPosition(tower) : undefined;
  }

  // --- sim-server connection -------------------------------------------------
  // One authoritative snapshot stream; this client only renders it. Every
  // subsystem that reacts to a snapshot is fanned out from here.
  connectSimServer((snapshot) => {
    reconcileBalloons(snapshot.balloons);
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

  // --- Balloon inspector panel (top-right, shown on selection) ---------------
  inspectorPanel = document.createElement('div');
  inspectorPanel.style.cssText = `
    position: fixed; top: 10px; right: 10px; z-index: 1000; display: none;
    background: rgba(20, 20, 20, 0.8); color: #fff;
    font: 12px sans-serif; padding: 10px 12px; border-radius: 6px;
    width: 240px; border: 1px solid rgba(63, 208, 255, 0.5);
  `;
  inspectorPanel.innerHTML = `
    <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:8px;">
      <span id="inspectorTitle" style="font-weight:bold; color:#3fd0ff;">Balloon</span>
      <button id="inspectorClose" title="Deselect" style="line-height:1;">&times;</button>
    </div>
    <div id="inspectorBody" style="display:flex; flex-direction:column; gap:4px;"></div>
  `;
  document.body.appendChild(inspectorPanel);
  inspectorBody = inspectorPanel.querySelector('#inspectorBody');
  inspectorTitle = inspectorPanel.querySelector('#inspectorTitle');
  inspectorPanel.querySelector('#inspectorClose').addEventListener('click', deselectBalloon);
  // Delegated: inspectorBody's innerHTML is fully rebuilt every snapshot
  // (~50ms), so a listener bound directly to #commsReplayBtn would need
  // rebinding just as often.
  inspectorBody.addEventListener('click', (e) => {
    if (e.target.id === 'commsReplayBtn' && selectedBalloonId !== null) {
      fetchAndAnimateComms(selectedBalloonId);
    }
  });

  // --- Comms log panel (bottom-right, shown alongside the inspector) --------
  // New DOM, same precedent as the inspector panel itself (MESH_COMMS_DESIGN.md
  // §3: "net-new DOM; the Controls panel is the only precedent").
  commsLogPanel = document.createElement('div');
  commsLogPanel.style.cssText = `
    position: fixed; bottom: 10px; right: 10px; z-index: 1000; display: none;
    background: rgba(20, 20, 20, 0.85); color: #fff;
    font: 11px sans-serif; padding: 10px 12px; border-radius: 6px;
    width: 420px; max-height: 220px; overflow-y: auto;
    border: 1px solid rgba(63, 208, 255, 0.5);
  `;
  commsLogPanel.innerHTML = `
    <div style="font-weight:bold; margin-bottom:6px; color:#3fd0ff;">Comms log</div>
    <table style="width:100%; border-collapse:collapse;">
      <thead>
        <tr style="opacity:0.6; text-align:left;">
          <th style="font-weight:normal;">Round</th>
          <th style="font-weight:normal;">Seq</th>
          <th style="font-weight:normal;">Hops</th>
          <th style="font-weight:normal;">Channel</th>
          <th style="font-weight:normal;">Ack</th>
          <th style="font-weight:normal;" title="Needs C3's hash chain">Hash</th>
          <th style="font-weight:normal;" title="Needs C3's hash chain">Tamper</th>
        </tr>
      </thead>
      <tbody id="commsLogTableBody"></tbody>
    </table>
  `;
  document.body.appendChild(commsLogPanel);
  commsLogTableBody = commsLogPanel.querySelector('#commsLogTableBody');

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
    useGlyphs = glyphsToggle.checked;
    updateBalloonRenderModeForAll();
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
  // Mutually exclusive with the belief overlay — both recolor every balloon,
  // and showing two overlays at once would just make each one illegible.
  beliefOverlayToggle.addEventListener('change', () => {
    beliefOverlayEnabled = beliefOverlayToggle.checked;
    beliefLegend.style.display = beliefOverlayEnabled ? 'block' : 'none';
    if (beliefOverlayEnabled && deliveryOverlayToggle.checked) {
      deliveryOverlayToggle.checked = false;
      deliveryOverlayEnabled = false;
      deliveryLegend.style.display = 'none';
    }
    for (const id of balloonEntities.keys()) applyBalloonColor(id);
  });
  deliveryOverlayToggle.addEventListener('change', () => {
    deliveryOverlayEnabled = deliveryOverlayToggle.checked;
    deliveryLegend.style.display = deliveryOverlayEnabled ? 'block' : 'none';
    if (deliveryOverlayEnabled && beliefOverlayToggle.checked) {
      beliefOverlayToggle.checked = false;
      beliefOverlayEnabled = false;
      beliefLegend.style.display = 'none';
    }
    for (const id of balloonEntities.keys()) applyBalloonColor(id);
  });

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
