import { params } from '../config.js';
import { BELIEF_CSS, BELIEF_LEGEND, DELIVERY_CSS, DELIVERY_LEGEND } from '../overlays.js';
import { OVERLAY_NONE, OVERLAY_BELIEF, OVERLAY_DELIVERY } from '../balloonLayer.js';

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
const MESH_DEGREE_BAR_MAX = 10; // full-width degree; threshold lands at 45%

// Our own POST and the next broadcast snapshot race: a snapshot computed just
// before the server applied our change would otherwise snap a slider back to
// the old value for a tick before jumping forward again, which reads as the
// slider "resisting" quick successive changes.
const REMOTE_SYNC_COOLDOWN_MS = 600;

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
// coloured in one place and captioned in another. `withValue` adds the
// percentage readout the belief legend carries and the delivery one doesn't.
function legendRows(keys, cssTable, labels, withValue) {
  return keys
    .map((key) => {
      const id = withValue
        ? ` id="belief${key[0].toUpperCase()}${key.slice(1)}Value"`
        : '';
      const value = withValue ? `<span${id}>&ndash;</span>` : '';
      return `
          <div style="${SPREAD}">
            <span><span style="color:${cssTable[key]};">&#9679;</span> ${labels[key]}</span>
            ${value}
          </div>`;
    })
    .join('');
}

export class ControlPanel {
  constructor({
    onPauseToggle,
    onHorizonInput,
    onHorizonCommit,
    onBalloonCountCommit,
    onGlyphsChange,
    onOverlayChange,
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
    // fighting a slider the user is actively dragging — or just released.
    this.draggingHorizon = false;
    this.draggingBalloons = false;
    this.horizonCooldownUntil = 0;
    this.balloonsCooldownUntil = 0;

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
          ${legendRows(['ok', 'stale', 'unaware', 'none'], BELIEF_CSS, BELIEF_LEGEND, true)}
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
        <div id="deliveryLegend" style="display:none; margin-top:5px; opacity:0.85;">
          ${legendRows(['radio', 'satellite', 'none'], DELIVERY_CSS, DELIVERY_LEGEND, false)}
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
      this.horizonCooldownUntil = Date.now() + REMOTE_SYNC_COOLDOWN_MS;
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
      this.balloonsCooldownUntil = Date.now() + REMOTE_SYNC_COOLDOWN_MS;
      const requested = parseInt(this.balloonsSlider.value, 10);
      params.numBalloons = requested;
      onBalloonCountCommit(requested);
    });

    // --- glyphs ---
    const glyphsToggle = $('#glyphsToggle');
    glyphsToggle.addEventListener('change', () => onGlyphsChange(glyphsToggle.checked));

    // --- mesh health readout ---
    this.meshDegreeValue = $('#meshDegreeValue');
    this.meshDegreeBar = $('#meshDegreeBar');
    this.meshGroundedValue = $('#meshGroundedValue');
    this.beliefValues = {
      ok: $('#beliefOkValue'),
      stale: $('#beliefStaleValue'),
      unaware: $('#beliefUnawareValue'),
      none: $('#beliefNoneValue'),
    };

    // --- overlays ---
    // The two are mutually exclusive: both recolor every balloon, and showing
    // two at once would just make each illegible. Driving a single mode off
    // whichever box was ticked enforces that by construction, rather than each
    // handler remembering to un-tick the other.
    const beliefToggle = $('#beliefOverlayToggle');
    const deliveryToggle = $('#deliveryOverlayToggle');
    const beliefLegend = $('#beliefLegend');
    const deliveryLegend = $('#deliveryLegend');
    const applyOverlay = (mode) => {
      beliefToggle.checked = mode === OVERLAY_BELIEF;
      deliveryToggle.checked = mode === OVERLAY_DELIVERY;
      beliefLegend.style.display = mode === OVERLAY_BELIEF ? 'block' : 'none';
      deliveryLegend.style.display = mode === OVERLAY_DELIVERY ? 'block' : 'none';
      onOverlayChange(mode);
    };
    beliefToggle.addEventListener('change', () =>
      applyOverlay(beliefToggle.checked ? OVERLAY_BELIEF : OVERLAY_NONE)
    );
    deliveryToggle.addEventListener('change', () =>
      applyOverlay(deliveryToggle.checked ? OVERLAY_DELIVERY : OVERLAY_NONE)
    );

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
    const now = Date.now();
    if (typeof snapshot.paused === 'boolean' && snapshot.paused !== this.paused) {
      this.paused = snapshot.paused;
      this._updatePauseButton();
    }
    if (
      !this.draggingHorizon &&
      now >= this.horizonCooldownUntil &&
      typeof snapshot.horizonRefractionCoeff === 'number'
    ) {
      const coeff = snapshot.horizonRefractionCoeff;
      if (coeff !== params.horizonRefractionCoeff) {
        params.horizonRefractionCoeff = coeff;
        this.horizonSlider.value = coeff;
        this.horizonValue.textContent = coeff.toFixed(2);
        onHorizonEcho();
      }
    }
    if (!this.draggingBalloons && now >= this.balloonsCooldownUntil) {
      const n = snapshot.balloons.length;
      if (n !== params.numBalloons) {
        params.numBalloons = n;
        this.balloonsSlider.value = n;
        this.balloonsValue.textContent = n;
      }
    }
    this._updateMeshHealth(snapshot);
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
