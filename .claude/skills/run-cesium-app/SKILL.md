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
