import {
  beliefKey,
  deliveryKey,
  bundleOutcome,
  commsAckLabel,
  BELIEF_CSS,
  BELIEF_VERDICT,
  DELIVERY_CSS,
  DELIVERY_LEGEND,
  COMMS_OUTCOME_CSS,
  MUTED_CSS,
} from '../overlays.js';

// ---------------------------------------------------------------------------
// InspectorPanel — the per-balloon readout shown on selection, plus the comms
// log that sits below it (docs/design/MESH_COMMS_DESIGN.md §3).
//
// No Cesium import: it renders snapshot data and the comms query response as
// HTML, nothing more. Colors arrive from overlays.js as CSS strings for the
// same reason.
// ---------------------------------------------------------------------------

// "82.32 W", "29.65 N" — same convention as the tower labels (towerModel.js).
export const fmtLon = (lon) => `${Math.abs(lon).toFixed(3)}° ${lon < 0 ? 'W' : 'E'}`;
export const fmtLat = (lat) => `${Math.abs(lat).toFixed(3)}° ${lat < 0 ? 'S' : 'N'}`;

const ROW = 'display:flex; justify-content:space-between;';

function row(label, value, css) {
  const styled = css ? ` style="color:${css};"` : '';
  return `<div style="${ROW}"><span>${label}</span><span${styled}>${value}</span></div>`;
}

// A record's channel is null until its bundle resolves, so anything that isn't
// one of the two real channels reads as "—", not as a state. Note this column
// is tinted from the *outcome* palette, not the delivery one — radio reads as
// the acked green rather than the overlay's lime.
function commsLogRowHtml(record) {
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

export class InspectorPanel {
  // `onClose` deselects; `onReplay` re-runs the packet animation for the
  // currently selected balloon; `onTraceToggle` starts/stops drawing its
  // flight path.
  constructor({ onClose, onReplay, onTraceToggle }) {
    this.comms = null; // last-fetched GET /api/balloons/:id/comms response
    // What the running protocol can express. Null until the first snapshot;
    // treated as "everything" until then, since this only ever hides rows.
    this.caps = null;

    this.panel = document.createElement('div');
    this.panel.style.cssText = `
    position: fixed; top: 10px; right: 10px; z-index: 1000; display: none;
    background: rgba(20, 20, 20, 0.8); color: #fff;
    font: 12px sans-serif; padding: 10px 12px; border-radius: 6px;
    width: 240px; border: 1px solid rgba(63, 208, 255, 0.5);
  `;
    this.panel.innerHTML = `
    <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:8px;">
      <span id="inspectorTitle" style="font-weight:bold; color:#3fd0ff;">Balloon</span>
      <button id="inspectorClose" title="Deselect" style="line-height:1;">&times;</button>
    </div>
    <div id="inspectorBody" style="display:flex; flex-direction:column; gap:4px;"></div>
  `;
    document.body.appendChild(this.panel);
    this.body = this.panel.querySelector('#inspectorBody');
    this.title = this.panel.querySelector('#inspectorTitle');
    this.panel.querySelector('#inspectorClose').addEventListener('click', onClose);
    // Delegated: the body's innerHTML is fully rebuilt every snapshot (~50ms),
    // so a listener bound directly to #commsReplayBtn would need rebinding
    // just as often.
    //
    // 'pointerdown', not 'click': a real click's mousedown and mouseup can
    // straddle one of those rebuilds, swapping the button out from under the
    // cursor mid-gesture — browsers then have no coherent element to fire
    // 'click' on, and the press is silently dropped. 'pointerdown' fires the
    // instant the button is pressed, synchronously, before any later rebuild
    // can interleave, so it can't be raced out from under a real click.
    this.body.addEventListener('pointerdown', (e) => {
      if (e.target.id === 'commsReplayBtn') onReplay();
      if (e.target.id === 'traceToggleBtn') onTraceToggle();
    });

    // Comms log panel (bottom-right, shown alongside the inspector): a row per
    // retained telemetry record, newest first — round · seq · hops · channel ·
    // ack state, plus hash-prefix/tamper columns kept as explicit placeholders
    // (C3's hash chain hasn't landed yet, so there's nothing honest to show
    // there beyond "—"). New DOM, same precedent as the inspector itself.
    this.logPanel = document.createElement('div');
    this.logPanel.style.cssText = `
    position: fixed; bottom: 10px; right: 10px; z-index: 1000; display: none;
    background: rgba(20, 20, 20, 0.85); color: #fff;
    font: 11px sans-serif; padding: 10px 12px; border-radius: 6px;
    width: 420px; max-height: 220px; overflow-y: auto;
    border: 1px solid rgba(63, 208, 255, 0.5);
  `;
    this.logPanel.innerHTML = `
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
    document.body.appendChild(this.logPanel);
    this.logTableBody = this.logPanel.querySelector('#commsLogTableBody');
  }

  // See ControlPanel.applyCapabilities — same reasoning, applied to the rows
  // of this panel rather than to the overlay toggles.
  applyCapabilities(caps) {
    this.caps = caps;
  }

  show(id) {
    this.panel.style.display = 'block';
    this.title.textContent = `Balloon #${id}`;
  }

  hide() {
    this.panel.style.display = 'none';
    this.clearComms();
  }

  setComms(comms) {
    this.comms = comms;
    const log = comms && comms.log;
    if (!log || log.length === 0) {
      this.logPanel.style.display = 'none';
      return;
    }
    this.logPanel.style.display = 'block';
    this.logTableBody.innerHTML = log.map(commsLogRowHtml).join('');
  }

  clearComms() {
    this.comms = null;
    this.logPanel.style.display = 'none';
  }

  // Text summary of the last-bundle animation, matching its colors. Reads from
  // the comms response fetched once on selection, not the per-snapshot
  // balloon — so this stays stable across the ~50ms cadence that rebuilds the
  // rest of the panel.
  _commsSummaryHtml() {
    if (this.comms === undefined || this.comms === null) {
      return `<div style="opacity:0.6;">Loading&hellip;</div>`;
    }
    const lb = this.comms.lastBundle;
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
    return (
      row('Seq', lb.seq) +
      row('Hops', total) +
      row('Outcome', outcomeText, COMMS_OUTCOME_CSS[outcome])
    );
  }

  // `b` is the selected balloon in the latest snapshot, or null if it isn't in
  // the active set any more. `isTracing` reflects TrailLayer's own state
  // (the source of truth) rather than anything this panel remembers itself.
  update(b, isTracing) {
    if (!b) {
      this.body.innerHTML =
        `<div style="opacity:0.7;">Not in the active set right now (raise the balloon count to bring it back).</div>`;
      return;
    }
    // Deliberately shows the balloon's belief and the truth as two separate
    // rows: the balloon acts on the former and has no access to the latter.
    const hasBelief = !this.caps || this.caps.routeBelief;
    const key = beliefKey(b);
    const beliefText =
      b.believedHops === null || b.believedHops === undefined
        ? 'no route known'
        : `${b.believedHops} hop${b.believedHops === 1 ? '' : 's'} to a tower`;
    // Under a protocol with no route belief these three rows have no referent
    // — "Believes: no route known" would read as a finding rather than as an
    // absent concept. Ground truth stays: it is the simulator's, not the
    // protocol's.
    const beliefRows = hasBelief
      ? `<div style="${ROW} margin-top:6px;"><span>Believes</span><span>${beliefText}</span></div>
      ${row('Actually grounded', b.grounded ? 'yes' : 'no')}
      ${row('Verdict', BELIEF_VERDICT[key], BELIEF_CSS[key])}`
      : row('Actually grounded', b.grounded ? 'yes' : 'no');
    const dKey = deliveryKey(b);
    const traceBtnHtml = isTracing
      ? `<button id="traceToggleBtn" title="Stop tracing this balloon's path" style="font-size:11px; padding:1px 6px; cursor:pointer; color:#ffe14d; border-color:#ffe14d;">&#9679; Tracing&hellip; stop</button>`
      : `<button id="traceToggleBtn" title="Draw this balloon's flight path in yellow" style="font-size:11px; padding:1px 6px; cursor:pointer;">Trace path</button>`;
    this.body.innerHTML = `
      <div style="${ROW} align-items:center;">
        <span>Flight path</span>
        ${traceBtnHtml}
      </div>
      ${row('Latitude', fmtLat(b.lat))}
      ${row('Longitude', fmtLon(b.lon))}
      ${row('Altitude', `${(b.alt / 1000).toFixed(2)} km`)}
      ${beliefRows}
      ${row('Last delivered via', DELIVERY_LEGEND[dKey], DELIVERY_CSS[dKey] ?? MUTED_CSS)}
      <div style="border-top: 1px solid rgba(255,255,255,0.2); margin-top:8px; padding-top:6px;">
        <div style="${ROW} align-items:center; margin-bottom:2px;">
          <span style="font-weight:bold;">Last bundle</span>
          ${
            !this.caps || this.caps.nextHopPaths
              ? `<button id="commsReplayBtn" title="Replay the animation" style="font-size:11px; padding:1px 6px; cursor:pointer;">&#8635; Replay</button>`
              : ''
          }
        </div>
        ${this._commsSummaryHtml()}
      </div>
      <div style="margin-top:6px; opacity:0.55; font-style:italic; line-height:1.4;">
        Measurements (gas, ballast, temperature, humidity) and the message log /
        tamper chain will appear here once those systems are built — see
        docs/design/MESH_COMMS_DESIGN.md.
      </div>
    `;
  }
}
