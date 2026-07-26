---
name: run-cesium-app
description: Drive cesium-app headlessly (Playwright + Chromium) to check rendering/behavior without a live browser — screenshots, JS eval, clicks, console capture.
---

# run-cesium-app

Headless driver for checking cesium-app's actual rendered behavior — not
just whether the code compiles/type-checks, but whether balloons, towers,
and radio links actually show up correctly in the browser.

## Prerequisites

The dev server (and, if the feature under test needs it, `sim-server` and
`wind_backend.py`) must already be running:

```bash
# terminal 1 — wind data
cd weather-data-server && ./run.sh

# terminal 2 — sim server
cd sim-server && cargo run --release

# terminal 3 — frontend
npm run dev   # http://localhost:5173/
```

This skill only drives a headless browser against whatever's already
serving at those URLs — it doesn't start them for you.

`wind_backend.py` is optional for most checks. Without it sim-server logs a
warning and runs on zero wind, which means balloons hold their longitude and
latitude (they still drift vertically, so links still form and change). Skip
it unless the thing under test involves wind, and you avoid the ~55s fetch.

## Changed Rust? Rebuild *and* restart, or you're testing the old binary

The single most common way to waste an hour here. `cargo build --release`
writes a new binary but does **not** affect the sim-server process already
running — that process keeps serving the old code, so your change appears to
do nothing and you go looking for a bug that isn't there.

```bash
cd sim-server && cargo build --release && cargo test   # build + prove it
pkill -f 'target/release/sim-server'                   # or ask the user
sleep 2
(setsid nohup ./target/release/sim-server > /tmp/sim-server.log 2>&1 < /dev/null &)
sleep 5
ss -ltnp | grep ':8080'                                # confirm the new PID owns it
```

Note the PID before and after — if it didn't change, the kill didn't work and
the old binary is still bound to `:8080`. A new sim-server that finds the port
occupied **panics and exits**, silently leaving you on the old one.

The frontend needs none of this: vite hot-reloads `src/` changes on save.

If the user is running the stack themselves (e.g. via `run-all.sh`), ask them
to restart rather than killing their processes out from under them.

## Usage

No tmux in this environment, so the driver is controlled by piping a
heredoc of commands to its stdin, one command per line, run strictly in
order. Each command prints exactly one `OK ...` / `ERR ...` /
`EVAL_RESULT ...` / `CONSOLE_DUMP ...` line when it finishes.

```bash
node .claude/skills/run-cesium-app/driver.mjs << 'EOF'
goto http://localhost:5173/
wait 60000
screenshot /tmp/app.png
eval document.querySelectorAll('canvas').length
console
quit
EOF
```

Run this via the Bash tool with a generous timeout — the whole heredoc
executes as one blocking command until `quit` is reached.

## Commands

- `goto [url]` — navigate (default `http://localhost:5173/`)
- `wait <ms>` — sleep
- `screenshot <path>` — save a PNG
- `eval <js expression>` — evaluate in page context (has access to
  `window`, `document`, etc.), prints `EVAL_RESULT <json>`
- `click <selector>` — CSS selector click
- `console` — dump captured `console.*` and uncaught page-error messages
  seen so far, as `CONSOLE_DUMP <json array>`
- `quit` / `exit` — close the browser, end the process

## Key timing fact

Globe, balloons, towers, and radio links all typically appear together
within about 10 seconds of `goto` — dominated by Cesium's terrain load, not
by the simulation (balloons/towers/links come from `sim-server` over
WebSocket and render on the very first snapshot, no "settling" period).
`wait 10000`-`15000` is usually enough; go longer (`20000`+) if terrain
load is slow that run, or if you specifically want to see radio-link
clusters change over time as balloons drift.

The "Wind vectors" panel control is the one thing that's still slow: it
fetches the full wind grid from `wind_backend.py` in the background
(measured ~356MB JSON, ~55s — a known perf issue, see the
`wind_backend_perf` memory) and shows "Loading wind data..." until that
resolves. It does not block anything else.

## What you can check with `eval`

The app doesn't expose a debug global by default. Useful checks without one:

- `document.querySelectorAll('canvas').length` — sanity check the Cesium
  viewer mounted at all.
- `document.body.innerText.includes('...')` — check panel text/labels.
- Console messages (via the `console` command) — this app logs meaningful
  state to the console, e.g. sim-server WebSocket connect/error/close
  events (`src/main.js`'s `connectSimServer()`). All simulation state
  (balloons, towers, links) is server-owned and arrives over that
  WebSocket — there's no local physics/link-detection code left to log
  from client-side.

If you need richer introspection (e.g. current balloon count, entity
positions), the cleanest path is temporarily stashing a reference on
`window` in the app code you're testing (e.g. `window.__debugViewer =
viewer;` near the top of `initCesium()`), driving with this skill, then
removing the stash — don't leave debug globals in committed code.

## Read server state directly — don't diagnose server bugs through the browser

`peek.mjs` (next to `driver.mjs`) reads one snapshot straight off sim-server's
WebSocket, with no browser in the loop:

```bash
node .claude/skills/run-cesium-app/peek.mjs
node .claude/skills/run-cesium-app/peek.mjs 's.meanDegree'
node .claude/skills/run-cesium-app/peek.mjs 's.balloons.filter(b => b.grounded).length'
```

No argument prints every scalar snapshot field plus array lengths; an argument
is a JS expression with `s` bound to the snapshot.

Reach for this the moment a browser reading looks wrong. Twice now a "bug"
turned out to be either browser lag or a mis-assumed parameter, and one
`peek.mjs` call would have settled it immediately. If the value is correct
here, the server is correct and the discrepancy is in the client or in the
lag described under Gotchas.

Corollary worth internalizing: **before explaining a discrepancy, read the
inputs.** A long hunt in this repo once came down to `horizonRefractionCoeff`
sitting at 3.55 because a slider had been moved, while the analysis assumed
the 4.12 default. Two hypotheses were built and discarded before anyone
checked the actual value.

## Logic the UI can't show: write an offline harness

Some behavior is invisible in the browser — convergence over hundreds of
ticks, state that decays slowly, anything needing a scenario you can't stage
by clicking. `sim-server` is a library (`src/lib.rs`), so a binary under
`src/bin/` can drive a real `World` directly, with no server, browser, or wind
backend:

```rust
let mut world = World::new(Arc::new(WindField::zero())); // frozen topology
world.add_tower(lon, lat, height);
world.spawn_balloon_pool(n);
world.set_visible_count(n);
for tick in 0..n_ticks { let snapshot = world.tick(60.0); /* assert/report */ }
```

Existing examples: `bin/mesh_depth.rs` (graph statistics) and
`bin/beacon_convergence.rs` (discovery convergence, plus deliberately breaking
the mesh to watch beliefs decay). The latter caught a protocol bug that no
amount of looking at the globe would have revealed — the give-away only showed
up in a scenario with every tower deleted. Prefer this over eyeballing
screenshots whenever the property you care about is temporal or statistical.

## Gotchas

- The driver process must stay alive for the whole command sequence — it's
  one Bash invocation, not a session you can send more commands to later.
  Plan the full command list before running it.
- `eval` runs via Playwright's `page.evaluate(string)`, so `arg` is
  evaluated as a JS expression/function body in the page's context, not
  Node's — no access to Node-side variables.
- If checking for a specific rendering bug (e.g. entities dropping out
  after a scene-mode change), wait for the "settle" period first, do the
  before-screenshot, trigger the interaction (`click`/`eval`), wait again,
  then take the after-screenshot — don't compare an unsettled state against
  a settled one.
- **Snapshot lag makes `eval` reads of server state stale.** This headless
  Chromium uses software GL (you'll see "GPU stall due to ReadPixels"
  warnings) and can't reconcile a few hundred balloons + links at the
  server's 20 Hz. WebSocket snapshots back up, so the page can be reflecting
  server state from *seconds* ago. If you `eval` a server-driven value (a
  slider echo, `paused`, a tick counter) shortly after triggering a change,
  you may read the pre-change value even though the server already applied
  it. Two defenses: (1) wait much longer than feels necessary (5–15s) before
  reading, and (2) for server-side logic, confirm against the server itself —
  `peek.mjs` (see above) is the fastest route, or the **sim-server log**, e.g.
  `run-all.sh` tees sim-server stderr to `"$LOG_DIR"/sim-server.log` (the
  `mktemp -d` path it prints as "Logs: …"; also `/tmp/tmp.*/sim-server.log`).
  A temporary `eprintln!` in the server also works. Don't conclude a server
  feature is broken from a short-wait browser read alone.
- **Stale / duplicate processes are the #1 time-sink.** Long sessions
  accumulate orphaned `sim-server` binaries (debug *and* release) and extra
  `vite` dev servers on different ports. Only one process can bind `:8080`,
  and a newly-launched sim-server that hits `AddrInUse` **panics and exits**,
  leaving the browser talking to an *old* binary without your changes — so
  your feature looks broken. Before trusting any run, confirm exactly what's
  live: `ss -ltnp | grep ':8080'` (which PID owns it) and
  `pgrep -af 'target/.*/sim-server'`. `run-all.sh` also *skips* starting
  sim-server if `:8080` is already occupied. When in doubt, kill all stray
  `sim-server`/`vite` processes and bring up a single clean stack. A giant
  tick counter in a snapshot (hours of ticks) is a tell that you're hitting a
  server from a previous session.
