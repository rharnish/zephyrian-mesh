import * as Cesium from 'cesium';

import { CommsReplay } from './commsReplay.js';
import { bundleOutcome } from './overlays.js';
import { fetchBalloonComms, setPaused } from './simClient.js';

// ---------------------------------------------------------------------------
// CaptureMode — tooling for recording README media. Opt-in via `?capture` (a
// 1280x720 viewport) or `?capture=WxH`; without that flag none of this loads.
//
// It finds a balloon whose last bundle was delivered and acked over several
// hops, freezes the sim, frames the path, and loops the replay. Recording
// reads the Cesium canvas directly (canvas.captureStream), so the output is
// exactly the globe — no panels, no cropping — and one recording is exactly
// one loop cycle, so it repeats seamlessly as a GIF.
//
// Keys:  f  find candidates, frame the best     n / p  next / previous
//        r  record one loop cycle (.webm)        s      screenshot (.png)
//        x  stop the replay (for stills)         u      toggle UI panels
//        space  pause / resume (the control panel's own binding)
// ---------------------------------------------------------------------------

export const DEFAULT_CAPTURE_SIZE = { width: 1280, height: 720 };

// Shortest path worth showing: origin -> relay -> tower-adjacent -> tower.
const MIN_LEGS = 3;
// Beyond this, more hops mostly shrink everything on screen.
const PREFERRED_MAX_LEGS = 6;
const COMMS_FETCH_CONCURRENCY = 8;
// Rest between loops. It is part of the recorded cycle, so the GIF opens and
// closes on the same empty frame.
const LOOP_GAP_MS = 900;
const RECORD_FPS = 30;

// Parses `?capture` / `?capture=WxH`. Returns null when capture mode is off.
export function captureSizeFromUrl(search) {
  const params = new URLSearchParams(search);
  if (!params.has('capture')) return null;
  const m = /^(\d+)x(\d+)$/.exec(params.get('capture') || '');
  return m ? { width: Number(m[1]), height: Number(m[2]) } : DEFAULT_CAPTURE_SIZE;
}

// Radio links are unordered, and the server's key order is its own business.
function linkKey(a, b) {
  return [a, b].sort().join('|');
}

async function mapLimit(items, limit, fn) {
  const out = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return out;
}

function download(blob, filename) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 5000);
}

function timestamp() {
  return new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-');
}

export class CaptureMode {
  constructor({ viewer, balloonLayer, positionOfTower, size }) {
    this.viewer = viewer;
    this.balloonLayer = balloonLayer;
    this.positionOfTower = positionOfTower;
    this.replay = new CommsReplay({ hopDurationMs: 750, pixelSize: 16, pathWidth: 4 });

    this.balloons = [];
    this.links = new Set();
    this.pausedWaiters = [];
    this.candidates = [];
    this.index = -1;
    this.loopToken = 0;
    this.loopTimer = null;
    this.recorder = null;
    this.recordNextCycle = false;

    this._layout(size);
    document.addEventListener('keydown', (e) => this._onKey(e));
    this._status('Press f to find a bundle to frame.');
  }

  // Pins the viewer to a fixed size so recordings come out the same size every
  // time, with the key help and status underneath — outside the canvas, so
  // never in a recording.
  _layout({ width, height }) {
    document.body.classList.add('capture-mode');
    const container = this.viewer.container;
    container.style.width = `${width}px`;
    container.style.height = `${height}px`;

    this.hud = document.createElement('div');
    this.hud.className = 'capture-hud';
    this.hud.innerHTML = `
      <div><b>capture mode</b> &nbsp; ${width}&times;${height}</div>
      <div class="capture-keys">
        <kbd>f</kbd> find &amp; frame &nbsp; <kbd>n</kbd>/<kbd>p</kbd> next/prev &nbsp;
        <kbd>r</kbd> record one loop &nbsp; <kbd>s</kbd> screenshot &nbsp;
        <kbd>x</kbd> stop replay &nbsp; <kbd>u</kbd> toggle panels &nbsp; <kbd>space</kbd> pause sim
      </div>
      <div class="capture-status"></div>`;
    document.body.appendChild(this.hud);
    this.statusEl = this.hud.querySelector('.capture-status');
    this.viewer.resize();
  }

  _status(text) {
    this.statusEl.textContent = text;
    console.log(`[capture] ${text}`);
  }

  handleSnapshot(snapshot) {
    this.balloons = snapshot.balloons;
    if (snapshot.edges) {
      this.links = new Set(snapshot.edges.map((e) => linkKey(...e.pairKey.split('|'))));
    }
    // Edges only ride snapshots in which they changed, so a paused world stops
    // sending them — the last set received is still the current one.
    if (snapshot.paused) {
      this.pausedWaiters.splice(0).forEach((resolve) => resolve());
    }
  }

  // Resolves on the first snapshot *drawn* after the server reports paused.
  // Asking to pause isn't enough: a client behind on snapshots keeps showing
  // the world moving for a while, and a scan made during that would pick links
  // that are gone by the time the loop is recorded.
  _pauseAndSettle() {
    setPaused(true);
    return new Promise((resolve) => this.pausedWaiters.push(resolve));
  }

  _onKey(e) {
    const tag = document.activeElement && document.activeElement.tagName;
    if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const actions = {
      f: () => this.findAndFrame(),
      n: () => this.step(1),
      p: () => this.step(-1),
      r: () => this.record(),
      s: () => this.screenshot(),
      x: () => this.stop(),
      u: () => document.body.classList.toggle('capture-hide-ui'),
    };
    const action = actions[e.key];
    if (action) {
      e.preventDefault();
      action();
    }
  }

  // Every balloon whose last bundle went out over radio and came back acked,
  // along a path that is still linked hop by hop right now. The replay draws a
  // past path at current positions, so a path whose links have since broken
  // would show the dot jumping across gaps — this keeps the picture honest.
  async findCandidates() {
    const radio = this.balloons.filter((b) => b.lastChannel === 'radio').map((b) => b.id);
    this._status(`Checking ${radio.length} radio-delivered balloons…`);
    const all = await mapLimit(radio, COMMS_FETCH_CONCURRENCY, fetchBalloonComms);

    const candidates = [];
    for (const comms of all) {
      const lb = comms && comms.lastBundle;
      if (!lb || !lb.path || lb.towerId == null || bundleOutcome(lb) !== 'acked') continue;
      const nodes = [...lb.path.map((id) => `b${id}`), `t${lb.towerId}`];
      const legs = nodes.length - 1;
      if (legs < MIN_LEGS) continue;
      let linked = true;
      for (let i = 0; i < legs && linked; i++) linked = this.links.has(linkKey(nodes[i], nodes[i + 1]));
      if (!linked) continue;
      candidates.push({ comms, legs });
    }
    // Most legs up to the preferred cap first; among equals, the lower id, so
    // the order is stable between runs of `f` on a frozen world.
    candidates.sort(
      (a, b) =>
        Math.min(b.legs, PREFERRED_MAX_LEGS) - Math.min(a.legs, PREFERRED_MAX_LEGS) ||
        a.legs - b.legs ||
        a.comms.id - b.comms.id
    );
    return candidates;
  }

  async findAndFrame() {
    // Freeze first, so the links checked are the links the replay is drawn on.
    this._status('Pausing the sim…');
    await this._pauseAndSettle();
    this.candidates = await this.findCandidates();
    if (this.candidates.length === 0) {
      this._status('No fully-linked acked multi-hop bundles right now — resume (space), wait, try f again.');
      return;
    }
    this.index = 0;
    this._frameCurrent();
  }

  step(delta) {
    if (this.candidates.length === 0) return this.findAndFrame();
    this.index = (this.index + delta + this.candidates.length) % this.candidates.length;
    this._frameCurrent();
  }

  _positions(comms) {
    const lb = comms.lastBundle;
    const positions = lb.path.map((id) => this.balloonLayer.positionOf(id));
    positions.push(this.positionOfTower(lb.towerId));
    return positions.every(Boolean) ? positions : null;
  }

  _frameCurrent() {
    const { comms, legs } = this.candidates[this.index];
    const positions = this._positions(comms);
    if (!positions) {
      this._status(`Balloon #${comms.id}'s path is no longer visible — press f to rescan.`);
      return;
    }
    this.stop();
    this.balloonLayer.setSelected(comms.id);

    // Looking north, tilted enough to read as a globe rather than a map, and
    // far enough back that the whole path fits with a margin.
    const sphere = Cesium.BoundingSphere.fromPoints(positions);
    const range = Math.max(sphere.radius * 3.2, 120000);
    this.viewer.camera.flyToBoundingSphere(sphere, {
      offset: new Cesium.HeadingPitchRange(0, Cesium.Math.toRadians(-50), range),
      duration: 1.5,
      complete: () => this._loop(),
    });
    this._status(
      `Candidate ${this.index + 1}/${this.candidates.length}: balloon #${comms.id}, ` +
        `${legs} legs to tower ${comms.lastBundle.towerId}. Sim paused.`
    );
  }

  _loop() {
    const token = ++this.loopToken;
    const { comms } = this.candidates[this.index];
    this.replay.clear(this.viewer);

    if (this.recordNextCycle) {
      this.recordNextCycle = false;
      this._startRecording();
    }

    const started = this.replay.render(
      this.viewer,
      comms,
      (id) => this.balloonLayer.positionOf(id),
      this.positionOfTower,
      () => {
        this.loopTimer = setTimeout(() => {
          if (token !== this.loopToken) return;
          this._stopRecording();
          this._loop();
        }, LOOP_GAP_MS);
      }
    );
    if (!started) {
      this._stopRecording();
      this._status(`Balloon #${comms.id}'s replay can't be drawn — press n or f.`);
    }
  }

  stop() {
    this.loopToken++;
    clearTimeout(this.loopTimer);
    this.replay.clear(this.viewer);
    this._stopRecording(true);
  }

  // Arms a recording of the next full loop cycle, which then starts from the
  // top — so the file begins as the path appears and ends on the rest after.
  record() {
    if (this.index < 0) {
      this._status('Frame a bundle first (f).');
      return;
    }
    if (this.recorder) return;
    this.recordNextCycle = true;
    this._status('Recording one loop…');
    this._loop();
  }

  _startRecording() {
    const stream = this.viewer.canvas.captureStream(RECORD_FPS);
    const mimeType = ['video/webm;codecs=vp9', 'video/webm'].find((t) => MediaRecorder.isTypeSupported(t));
    const chunks = [];
    const recorder = new MediaRecorder(stream, { mimeType, videoBitsPerSecond: 12_000_000 });
    recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
    recorder.onstop = () => {
      stream.getTracks().forEach((t) => t.stop());
      if (recorder.__discard) return;
      const id = this.candidates[this.index]?.comms.id;
      download(new Blob(chunks, { type: 'video/webm' }), `bundle-b${id}-${timestamp()}.webm`);
      this._status('Saved one loop to your Downloads folder.');
    };
    recorder.start();
    this.recorder = recorder;
  }

  _stopRecording(discard = false) {
    if (!this.recorder) return;
    this.recorder.__discard = discard;
    this.recorder.stop();
    this.recorder = null;
    if (discard) this._status('Recording discarded.');
  }

  // The drawing buffer is only guaranteed intact immediately after a render,
  // so read it from inside postRender rather than whenever the key lands.
  screenshot() {
    const scene = this.viewer.scene;
    const remove = scene.postRender.addEventListener(() => {
      remove();
      const dataUrl = scene.canvas.toDataURL('image/png');
      fetch(dataUrl)
        .then((r) => r.blob())
        .then((blob) => {
          download(blob, `globe-${timestamp()}.png`);
          this._status('Saved a screenshot to your Downloads folder.');
        });
    });
    scene.requestRender();
  }
}
