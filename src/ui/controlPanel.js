import { params } from '../config.js';
import { BELIEF_CSS, BELIEF_LEGEND, DELIVERY_CSS, DELIVERY_LEGEND, DELIVERY_MARK, deliveryMix } from '../overlays.js';
import { OVERLAY_NONE, OVERLAY_BELIEF } from '../balloonLayer.js';

// ---------------------------------------------------------------------------
// ControlPanel — the fixed overlay at top-left: pause, the two sliders, the
// overlay toggles and their legends, the mesh-health readout, and the
// wind-vector controls.
//
// Built in JS rather than in index.html so a new control needs no HTML edit.
// Every action is reported through a callback; the panel itself never talks to
// sim-server or to Cesium, so it holds no viewer and imports neither.
// ---------------------------------------------------------------------------

// Mean degree at which a random geometric graph percolates. Below it the mesh
// is islands, above it a giant component; see sim-server/src/bin/mesh_depth.rs
// for the measurement this comes from.
const PERCOLATION_DEGREE = 4.5;
// View-lag thresholds. Under a second is normal for a browser keeping up with
// the snapshot stream; a few seconds means it is not, and the globe is showing
// a world that has already moved on.
const VIEW_LAG_WARN_MS = 1000;
const VIEW_LAG_BAD_MS = 4000;
const MESH_DEGREE_BAR_MAX = 10; // full-width degree; threshold lands at 45%

// Our own request and the snapshot stream race. Snapshots already in flight
// when we sent a change still carry the old value, and applying one of those
// snaps the slider back to where it was before jumping forward again — the
// "bounce" on release.
//
// This used to be handled by ignoring snapshots for a fixed 600ms after a
// change, which does not work: the client reads snapshots from an unbounded
// queue, so wall-clock time here says nothing about *which* snapshot is being
// applied. Measured on this machine the browser can be hundreds of ticks
// behind and losing ground, at which point every stale snapshot in the backlog
// arrives long after any timer has expired. It also explained why shrinking
// the balloon count bounced and growing it didn't: shrinking means the queued
// stale snapshots are the larger, slower ones, so the backlog drains slower.
//
// So instead of guessing at a duration, we wait for proof: after requesting a
// value, ignore this control's snapshot value until a snapshot actually
// carries what we asked for. That is correct no matter how deep the backlog
// gets.
//
// The timeout is only a safety net for a request that is never answered at
// all — a dropped POST, or a server restart. It is deliberately far longer
// than any plausible confirmation delay, because the two failures are not
// symmetric: expiring early re-creates exactly the bounce this exists to
// prevent, while expiring late merely means a control ignores another tab's
// changes for a while longer after a command that already went missing.
//
// Sizing it needs the client's lag, not the server's. The server confirms in
// 30-130ms, but the client reads snapshots off a socket it cannot drain fast
// enough, so confirmation can surface many seconds after it was sent — on a
// software-GL machine here, longer than 5s even with only 150 balloons.
export const PENDING_REQUEST_TIMEOUT_MS = 30000;

// One in-flight "I asked the server for this value" for a single control.
// `matches` compares a snapshot's value against the requested one, since the
// horizon coefficient is a float and the balloon count an integer.
export class PendingRequest {
  constructor(matches) {
    this.matches = matches;
    this.value = null; // null when nothing is outstanding
    this.at = 0;
  }

  request(value) {
    this.value = value;
    this.at = Date.now();
  }

  // True while this snapshot's value should be ignored. Clears itself once the
  // server confirms, or once the request has gone unanswered long enough that
  // continuing to ignore would be worse than accepting whatever is there.
  shouldIgnore(snapshotValue) {
    if (this.value === null) return false;
    if (this.matches(snapshotValue, this.value)) {
      this.value = null;
      return false;
    }
    if (Date.now() - this.at > PENDING_REQUEST_TIMEOUT_MS) {
      this.value = null;
      return false;
    }
    return true;
  }
}

// Inline SVG (fill:currentColor) rather than the ⏸/▶ unicode glyphs, which
// render as orange emoji on most systems and ignore CSS `color`. currentColor
// is set to white on the button, so these match the panel's other controls.
const PAUSE_ICON =
  '<svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor" aria-hidden="true"><rect x="3" y="2" width="3" height="10"/><rect x="8" y="2" width="3" height="10"/></svg>';
const PLAY_ICON =
  '<svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor" aria-hidden="true"><path d="M3 2 L12 7 L3 12 Z"/></svg>';

const SECTION = 'border-top: 1px solid rgba(255,255,255,0.2); padding-top: 8px;';
const SPREAD = 'display:flex; justify-content:space-between;';
const CHECKBOX_ROW = 'display:flex; gap:6px; align-items:center;';

// Legend rows are generated from the palette so a new overlay state can't be
// coloured in one place and captioned in another. `idPrefix` names the span
// holding each row's percentage, and is what `valueRefs` below looks up.
function legendRows(keys, cssTable, labels, idPrefix, marks) {
  return keys
    .map((key) => {
      const id = `${idPrefix}${key[0].toUpperCase()}${key.slice(1)}Value`;
      // For delivery, show the glyph actually drawn on the globe rather than a
      // generic dot, so the legend is the key you read the map with.
      const swatch = marks ? marks[key] || '&mdash;' : '&#9679;';
      return `
          <div style="${SPREAD}">
            <span><span style="color:${cssTable[key]}; display:inline-block; width:11px;">${swatch}</span> ${labels[key]}</span>
            <span id="${id}">&ndash;</span>
          </div>`;
    })
    .join('');
}

// The spans legendRows just created, keyed the same way, so a caller updates
// them by overlay key rather than by remembering element ids.
function valueRefs(panel, keys, idPrefix) {
  return Object.fromEntries(
    keys.map((key) => [
      key,
      panel.querySelector(`#${idPrefix}${key[0].toUpperCase()}${key.slice(1)}Value`),
    ])
  );
}

const BELIEF_KEYS = ['ok', 'stale', 'unaware', 'none'];
const DELIVERY_KEYS = ['radio', 'satellite', 'none'];

export class ControlPanel {
  constructor({
    onPauseToggle,
    onHorizonInput,
    onHorizonCommit,
    onBalloonCountCommit,
    onGlyphsChange,
    onOverlayChange,
    onDeliveryMarksChange,
    onWindChange,
    windFieldPromise,
  }) {
    // Pause state. The server is the single source of truth: `paused` and the
    // button label are updated ONLY from snapshot.paused (see syncFromSnapshot).
    // A click/spacebar just reports the desired state and waits for the server
    // to echo it back. We deliberately do NOT flip `paused` optimistically —
    // doing so races the in-flight pre-change snapshots and makes the toggle
    // misfire (an earlier bug). One round-trip of latency on the label is a
    // fine price for a state that can't desync.
    this.paused = false;
    // Lets another tab's slider changes (arriving via snapshots, since the
    // server is the source of truth) update this tab's controls too, without
    // fighting a slider the user is actively dragging — or one whose change is
    // still making its way back through the snapshot stream.
    this.draggingHorizon = false;
    this.draggingBalloons = false;
    // Floats survive the JSON round trip intact at this slider's 0.05 step,
    // but compare with a tolerance rather than betting the control's behavior
    // on that.
    this.horizonRequest = new PendingRequest((a, b) => Math.abs(a - b) < 1e-9);
    this.balloonsRequest = new PendingRequest((a, b) => a === b);

    const panel = document.createElement('div');
    this.panel = panel;
    panel.style.cssText = `
    position: fixed; top: 10px; left: 10px; z-index: 1000;
    background: rgba(20, 20, 20, 0.75); color: #fff;
    font: 12px sans-serif; padding: 10px 12px; border-radius: 6px;
    display: flex; flex-direction: column; gap: 8px; width: 220px;
  `;
    panel.innerHTML = `
    <div style="${SPREAD} align-items:center;">
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
      <div style="${SECTION}">
        <label style="${SPREAD}">
          <span>Horizon coeff.</span>
          <span id="horizonCoeffValue">${params.horizonRefractionCoeff.toFixed(2)}</span>
        </label>
        <input id="horizonCoeffSlider" type="range" min="2.5" max="4.2" step="0.05"
               value="${params.horizonRefractionCoeff}" style="width: 100%;" />
      </div>
      <div style="${SECTION}">
        <label style="${SPREAD}">
          <span>Balloons</span>
          <span id="numBalloonsValue">${params.numBalloons}</span>
        </label>
        <input id="numBalloonsSlider" type="range" min="1" max="2000" step="10"
               value="${params.numBalloons}" style="width: 100%;" />
        <div style="${CHECKBOX_ROW} margin-top: 4px;">
          <input id="glyphsToggle" type="checkbox" />
          <label for="glyphsToggle" style="flex:1;">Glyphs</label>
        </div>
      </div>
      <!-- Mesh health. Both sliders above are really two ways of moving mean
           degree, and the mesh percolates around degree ~4.5 — below it the
           network shatters into islands, above it nearly everything reaches a
           tower. The tick mark is that threshold, so a drag shows how close
           the current settings are to falling apart. -->
      <div style="${SECTION}">
        <label style="${SPREAD}">
          <span>Mesh degree</span>
          <span id="meshDegreeValue">&ndash;</span>
        </label>
        <div style="position:relative; height:5px; margin:5px 0 6px;
                    background:rgba(255,255,255,0.15); border-radius:3px;">
          <div id="meshDegreeBar"
               style="height:100%; width:0%; border-radius:3px; background:#888;
                      transition: width 0.2s linear, background-color 0.2s linear;"></div>
          <div title="percolation threshold (degree ${PERCOLATION_DEGREE})"
               style="position:absolute; left:${(PERCOLATION_DEGREE / MESH_DEGREE_BAR_MAX) * 100}%; top:-3px; bottom:-3px; width:1px;
                      background:rgba(255,255,255,0.8);"></div>
        </div>
        <label style="${SPREAD}">
          <span>Grounded</span>
          <span id="meshGroundedValue">&ndash;</span>
        </label>
        <label style="${SPREAD}" title="How old the world you are looking at is. Rises when this browser cannot keep up with the snapshot stream.">
          <span>View lag</span>
          <span id="viewLagValue">&ndash;</span>
        </label>
      </div>
      <!-- Belief vs. truth. Balloons only know what beacons told them, so
           their belief lags reality (stale) or trails behind it (unaware).
           See MESH_COMMS_DESIGN.md §1. -->
      <div style="${SECTION}">
        <div style="${CHECKBOX_ROW}">
          <input id="beliefOverlayToggle" type="checkbox" />
          <label for="beliefOverlayToggle" style="flex:1;">Belief overlay</label>
        </div>
        <div id="beliefLegend" style="display:none; margin-top:5px; opacity:0.85;">
          ${legendRows(BELIEF_KEYS, BELIEF_CSS, BELIEF_LEGEND, 'belief')}
        </div>
      </div>
      <!-- Last-delivery overlay. Server truth about how each balloon's most
           recent bundle actually got through — radio mesh vs. satellite
           release valve. See MESH_COMMS_DESIGN.md §3/§4. -->
      <div style="${SECTION}">
        <div style="${CHECKBOX_ROW}">
          <input id="deliveryOverlayToggle" type="checkbox" />
          <label for="deliveryOverlayToggle" style="flex:1;">Last-delivery overlay</label>
        </div>
        <div id="deliveryLegend" style="margin-top:5px; opacity:0.85;">
          ${legendRows(DELIVERY_KEYS, DELIVERY_CSS, DELIVERY_LEGEND, 'delivery', DELIVERY_MARK)}
        </div>
      </div>
      <div style="${CHECKBOX_ROW} ${SECTION}">
        <input id="windVectorsToggle" type="checkbox" />
        <label for="windVectorsToggle" style="flex:1;">Wind vectors</label>
      </div>
      <div>
        <select id="windVectorLevel" style="width: 100%;" disabled></select>
      </div>
      <div>
        <label style="${SPREAD}">
          <span>Arrow density (stride)</span>
          <span id="windVectorStrideValue">12</span>
        </label>
        <input id="windVectorStride" type="range" min="2" max="12" step="1" value="12"
               style="width: 100%;" disabled />
      </div>
    </div>
  `;
    document.body.appendChild(panel);

    const $ = (sel) => panel.querySelector(sel);

    // --- collapse ---
    const panelBody = $('#panelBody');
    const collapseToggle = $('#panelCollapseToggle');
    collapseToggle.addEventListener('click', () => {
      const collapsed = panelBody.style.display === 'none';
      panelBody.style.display = collapsed ? 'flex' : 'none';
      panel.style.width = collapsed ? '220px' : 'auto';
      collapseToggle.textContent = collapsed ? '−' : '+';
      collapseToggle.title = collapsed ? 'Collapse' : 'Expand';
    });

    // --- pause ---
    this.pauseToggle = $('#pauseToggle');
    this._updatePauseButton();
    this.pauseToggle.addEventListener('click', () => onPauseToggle(!this.paused));
    // Spacebar toggles pause too — but not while typing in a form control.
    document.addEventListener('keydown', (e) => {
      if (e.code !== 'Space' && e.key !== ' ') return;
      const tag = document.activeElement && document.activeElement.tagName;
      if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA' || tag === 'BUTTON') return;
      e.preventDefault();
      onPauseToggle(!this.paused);
    });

    // --- horizon coefficient ---
    this.horizonSlider = $('#horizonCoeffSlider');
    this.horizonValue = $('#horizonCoeffValue');
    this.horizonSlider.addEventListener('input', () => {
      this.draggingHorizon = true;
      params.horizonRefractionCoeff = parseFloat(this.horizonSlider.value);
      this.horizonValue.textContent = params.horizonRefractionCoeff.toFixed(2);
      onHorizonInput(params.horizonRefractionCoeff);
    });
    this.horizonSlider.addEventListener('change', () => {
      // Sync only on release (not every drag tick) — this triggers a full grid
      // rebuild on the server, so it's not something to send on every 'input'.
      this.draggingHorizon = false;
      this.horizonRequest.request(params.horizonRefractionCoeff);
      onHorizonCommit(params.horizonRefractionCoeff);
    });

    // --- balloon count ---
    this.balloonsSlider = $('#numBalloonsSlider');
    this.balloonsValue = $('#numBalloonsValue');
    this.balloonsSlider.addEventListener('input', () => {
      this.draggingBalloons = true;
      this.balloonsValue.textContent = this.balloonsSlider.value; // live label, cheap
    });
    this.balloonsSlider.addEventListener('change', () => {
      // Release-only for the same reason: this is a full clear+respawn there.
      this.draggingBalloons = false;
      const requested = parseInt(this.balloonsSlider.value, 10);
      params.numBalloons = requested;
      this.balloonsRequest.request(requested);
      onBalloonCountCommit(requested);
    });

    // --- glyphs ---
    const glyphsToggle = $('#glyphsToggle');
    glyphsToggle.addEventListener('change', () => onGlyphsChange(glyphsToggle.checked));

    // --- mesh health readout ---
    this.meshDegreeValue = $('#meshDegreeValue');
    this.meshDegreeBar = $('#meshDegreeBar');
    this.meshGroundedValue = $('#meshGroundedValue');
    this.viewLagValue = $('#viewLagValue');
    this.beliefValues = valueRefs(panel, BELIEF_KEYS, 'belief');
    this.deliveryValues = valueRefs(panel, DELIVERY_KEYS, 'delivery');

    // --- overlays ---
    // These are no longer mutually exclusive. Belief owns the balloon tint;
    // delivery is a separate mark above each balloon carrying its own colour,
    // so both can be read at once — which is the point of the split.
    const beliefToggle = $('#beliefOverlayToggle');
    const deliveryToggle = $('#deliveryOverlayToggle');
    const beliefLegend = $('#beliefLegend');
    beliefToggle.addEventListener('change', () => {
      const mode = beliefToggle.checked ? OVERLAY_BELIEF : OVERLAY_NONE;
      beliefLegend.style.display = beliefToggle.checked ? 'block' : 'none';
      onOverlayChange(mode);
    });
    // The delivery legend stays visible either way: its percentages are a live
    // readout of the field, and its marks are the key for the globe.
    deliveryToggle.addEventListener('change', () => onDeliveryMarksChange(deliveryToggle.checked));

    // --- wind vectors ---
    // These stay disabled until the (slow, backgrounded) wind field resolves.
    const levelSelect = $('#windVectorLevel');
    const windToggle = $('#windVectorsToggle');
    const strideSlider = $('#windVectorStride');
    const strideValue = $('#windVectorStrideValue');
    windToggle.disabled = true;
    levelSelect.innerHTML = '<option>Loading wind data...</option>';

    const reportWind = () =>
      onWindChange({
        enabled: windToggle.checked,
        levelIndex: parseInt(levelSelect.value, 10),
        stride: parseInt(strideSlider.value, 10),
      });

    windFieldPromise.then((windField) => {
      levelSelect.innerHTML = '';
      windField.levels.forEach((level, idx) => {
        const opt = document.createElement('option');
        opt.value = idx;
        opt.textContent = `${level.pressureHpa} hPa (~${(level.altitudeM / 1000).toFixed(1)} km)`;
        levelSelect.appendChild(opt);
      });
      windToggle.disabled = false;
    });

    windToggle.addEventListener('change', () => {
      levelSelect.disabled = !windToggle.checked;
      strideSlider.disabled = !windToggle.checked;
      reportWind();
    });
    levelSelect.addEventListener('change', reportWind);
    strideSlider.addEventListener('change', () => {
      // 'change' (on release), not 'input' (every drag tick) — rebuilding the
      // whole arrow field is too expensive to do on every pixel of drag.
      strideValue.textContent = strideSlider.value;
      reportWind();
    });
    strideSlider.addEventListener('input', () => {
      strideValue.textContent = strideSlider.value; // live label, cheap
    });
  }

  _updatePauseButton() {
    // Media-player convention: show ⏸ while running (click to pause), ▶ while
    // paused (click to resume). Title carries the word for accessibility.
    this.pauseToggle.innerHTML = this.paused ? PLAY_ICON : PAUSE_ICON;
    this.pauseToggle.title = this.paused ? 'Resume' : 'Pause';
  }

  // `onHorizonEcho` fires when another tab's horizon change arrives, since the
  // tower range circles have to be rebuilt for it.
  syncFromSnapshot(snapshot, onHorizonEcho) {
    if (typeof snapshot.paused === 'boolean' && snapshot.paused !== this.paused) {
      this.paused = snapshot.paused;
      this._updatePauseButton();
    }
    if (!this.draggingHorizon && typeof snapshot.horizonRefractionCoeff === 'number') {
      const coeff = snapshot.horizonRefractionCoeff;
      // shouldIgnore is always evaluated — it is what clears the request once
      // the server confirms, so it must not be short-circuited away.
      const stale = this.horizonRequest.shouldIgnore(coeff);
      if (!stale && coeff !== params.horizonRefractionCoeff) {
        params.horizonRefractionCoeff = coeff;
        this.horizonSlider.value = coeff;
        this.horizonValue.textContent = coeff.toFixed(2);
        onHorizonEcho();
      }
    }
    if (!this.draggingBalloons) {
      // There is no count field on the snapshot — the visible balloons *are*
      // the count, so this doubles as the confirmation that a resize landed.
      const n = snapshot.balloons.length;
      const stale = this.balloonsRequest.shouldIgnore(n);
      if (!stale && n !== params.numBalloons) {
        params.numBalloons = n;
        this.balloonsSlider.value = n;
        this.balloonsValue.textContent = n;
      }
    }
    this._updateMeshHealth(snapshot);
    this._updateDeliveryMix(snapshot);
  }

  // Always on screen, so kept independent of the mesh-health readout and its
  // early returns. Derived from the balloons already in this snapshot rather
  // than sent as three more fields — every balloon's lastChannel is right
  // here, so a server-computed aggregate would restate what we hold.
  _updateDeliveryMix(snapshot) {
    const mix = deliveryMix(snapshot.balloons);
    for (const key of DELIVERY_KEYS) {
      this.deliveryValues[key].textContent = `${mix[key].toFixed(0)}%`;
    }
  }

  // Unlike the sliders, this readout is never user-driven — it just mirrors
  // whatever the server last measured, so there's no drag/cooldown guard.
  _updateMeshHealth(snapshot) {
    const degree = snapshot.meanDegree;
    const grounded = snapshot.groundedPct;
    if (typeof degree !== 'number' || typeof grounded !== 'number') return;

    this.meshDegreeValue.textContent = degree.toFixed(1);
    this.meshGroundedValue.textContent = `${grounded.toFixed(0)}%`;

    const pct = Math.min(100, (degree / MESH_DEGREE_BAR_MAX) * 100);
    this.meshDegreeBar.style.width = `${pct}%`;
    // Red well below the threshold, amber in the critical band either side of
    // it (where paths get long and delivery turns erratic), green above.
    const color =
      degree < PERCOLATION_DEGREE - 1 ? BELIEF_CSS.stale
      : degree < PERCOLATION_DEGREE + 1 ? BELIEF_CSS.unaware
      : BELIEF_CSS.ok;
    this.meshDegreeBar.style.backgroundColor = color;
    this.meshDegreeValue.style.color = color;

    // How old the world on screen is: the gap between when the server built
    // this snapshot and now. Rises without bound when the client cannot drain
    // the stream, which is otherwise invisible — a stale globe looks exactly
    // like a live one. Assumes both clocks agree, which holds while server and
    // browser are the same machine; across machines it still tracks *change*
    // even if the absolute number carries the skew.
    if (typeof snapshot.serverTimeMs === 'number') {
      const lagMs = Math.max(0, Date.now() - snapshot.serverTimeMs);
      this.viewLagValue.textContent =
        lagMs < VIEW_LAG_WARN_MS ? `${lagMs} ms` : `${(lagMs / 1000).toFixed(1)} s`;
      this.viewLagValue.style.color =
        lagMs >= VIEW_LAG_BAD_MS ? BELIEF_CSS.stale
        : lagMs >= VIEW_LAG_WARN_MS ? BELIEF_CSS.unaware
        : BELIEF_CSS.ok;
    }

    if (typeof snapshot.believedGroundedPct !== 'number') return;
    // "believes and is right" is everything that believes, minus those whose
    // belief is stale; the remainder with no belief splits into unaware (a
    // route exists) and none.
    const stale = snapshot.beliefStalePct;
    const unaware = snapshot.beliefUnawarePct;
    const ok = snapshot.believedGroundedPct - stale;
    this.beliefValues.ok.textContent = `${ok.toFixed(0)}%`;
    this.beliefValues.stale.textContent = `${stale.toFixed(0)}%`;
    this.beliefValues.unaware.textContent = `${unaware.toFixed(0)}%`;
    this.beliefValues.none.textContent = `${Math.max(0, 100 - ok - stale - unaware).toFixed(0)}%`;
  }
}
