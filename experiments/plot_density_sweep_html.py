#!/usr/bin/env python3
"""Interactive HTML twin of plot_density_sweep.py's PNG — same CSV, same
series, same color language, but with a crosshair, per-n tooltips, a legend
that isolates a series on click, and a data table.

The PNG stays the canonical figure for `aggregation-summary.md` (Markdown can
embed it; GitHub will not run this file's script). This exists for the case the
PNG is bad at: eleven protocols on one pair of axes, where "which line is that,
and what is it actually worth at n=1200" is a question you have to squint at a
static image to answer. Hovering answers it directly, and clicking a legend
entry drops the other ten.

Encoding is deliberately identical to the PNG's, because the two are read side
by side: hue is protocol *family*, weight/dash is *role* (headline contender
solid and full-weight; every other variant thin and dashed in its family's
hue), and the ground-truth reachability ceiling is the app's own belief-vs-truth
green, dotted, so it reads as a reference line rather than a twelfth contender.

Color is the light palette plot_density_sweep.py already validated (dataviz
categorical slots 1/2/3/4/8) plus that palette's documented dark steps. Both
modes pass the adjacent-pair gates except one dark warn (yellow<->red CVD dE 6.2,
inside the 6-8 band); that band is legal only with secondary encoding, which
this chart has three of — dash/weight per role, direct end labels, and the
tooltip's numbers. The light mode's aqua and yellow sit under 3:1 on the light
surface, whose documented relief is visible labels or a table view; both ship.

Usage:
    python3 experiments/plot_density_sweep_html.py experiments/density-sweep-results.csv \
        --out experiments/protocol-results/density-sweep.html \
        --truth experiments/ground-truth-sweep-results.csv
"""

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from plot_density_sweep import (  # noqa: E402
    LABEL_ORDER,
    load,
    load_truth,
    mean,
    sem,
)

# Light steps are plot_density_sweep.py's, unchanged, so the HTML and the PNG
# name the same protocol with the same hue. Dark steps are the same five hues
# re-stepped for the dark surface (dataviz palette.md's dark column), not a
# second palette -- with slot 8's light red kept rather than its dark step,
# which lands too close to yellow once slots 5-7 aren't on screen between them.
LIGHT = {
    "blue": "#2a78d6",
    "orange": "#eb6834",
    "aqua": "#1baf7a",
    "yellow": "#eda100",
    "red": "#e34948",
    "truth": "#5fd08a",
}
DARK = {
    "blue": "#3987e5",
    "orange": "#d95926",
    "aqua": "#199e70",
    "yellow": "#c98500",
    "red": "#e34948",
    "truth": "#5fd08a",
}

# label -> (hue key, is_headline), mirroring plot_density_sweep.py's STYLE.
STYLE = {
    "dv-dtn": ("blue", True),
    "digest-mesh4": ("orange", True),
    "spray-l16": ("red", True),
    "spray-l4": ("red", False),
    "reactive": ("yellow", True),
    "reactive-tower": ("yellow", False),
    "reactive-overhear": ("yellow", False),
    "reactive-ring": ("yellow", False),
    "reactive-overhear-ring": ("yellow", False),
    "linkstate": ("aqua", True),
    "linkstate-mpr": ("aqua", False),
}

TRUTH_KEY = "__truth__"
TRUTH_LABEL = "ground truth (reachable)"
# The end-of-line label lives in the right margin, where the legend's full
# wording doesn't fit; only the truth line needs a shorter form.
TRUTH_END_LABEL = "truth"

PANELS = [
    ("completion_rate", "Completion rate (%)", "delivered / resolved"),
    ("delivered_per_originated", "Delivered (%)", "delivered / originated"),
]


def build_series(data, truth, ns):
    """-> (series_meta, series_data) aligned to `ns`.

    series_data[key][metric] = {"mean": [...], "lo": [...], "hi": [...]},
    with None wherever a cell wasn't swept, so the line breaks rather than
    interpolating across a gap (same rule as plot_sweep_html.py).
    """
    meta, out = [], {}

    if truth:
        # Ground truth is protocol-independent, so the same curve is drawn on
        # both panels -- it is the percolation ceiling for either denominator.
        m = [mean(truth[n]) if n in truth else None for n in ns]
        e = [sem(truth[n]) if n in truth else None for n in ns]
        band = {
            "mean": m,
            "lo": [None if v is None else v - s for v, s in zip(m, e)],
            "hi": [None if v is None else v + s for v, s in zip(m, e)],
        }
        out[TRUTH_KEY] = {key: band for key, _, _ in PANELS}
        meta.append({"key": TRUTH_KEY, "label": TRUTH_LABEL, "end": TRUTH_END_LABEL,
                     "hue": "truth", "headline": True, "dash": "2 3", "band": True})

    for label in LABEL_ORDER:
        if label not in data:
            continue
        hue, headline = STYLE[label]
        per_metric = {}
        for key, _, _ in PANELS:
            m, lo, hi = [], [], []
            for n in ns:
                vals = data[label].get(n, {}).get(key)
                if not vals:
                    m.append(None), lo.append(None), hi.append(None)
                    continue
                mu, se = 100.0 * mean(vals), 100.0 * sem(vals)
                m.append(round(mu, 4)), lo.append(round(mu - se, 4)), hi.append(round(mu + se, 4))
            per_metric[key] = {"mean": m, "lo": lo, "hi": hi}
        out[label] = per_metric
        meta.append({"key": label, "label": label, "end": label, "hue": hue,
                     "headline": headline, "dash": None if headline else "4 3",
                     "band": headline})

    return meta, out


TEMPLATE = r"""<title>__TITLE__</title>
<style>
  .viz-root {
    color-scheme: light;
    --surface-1:      #fcfcfb;
    --page:           #f9f9f7;
    --text-primary:   #0b0b0b;
    --text-secondary: #52514e;
    --text-muted:     #898781;
    --grid:           #e1e0d9;
    --baseline:       #c3c2b7;
    --border:         rgba(11,11,11,0.10);
    --hover:          rgba(11,11,11,0.04);
__LIGHT_VARS__
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
    background: var(--page);
    color: var(--text-primary);
    padding: 32px 16px;
    box-sizing: border-box;
    min-height: 100vh;
  }
  @media (prefers-color-scheme: dark) {
    :root:where(:not([data-theme="light"])) .viz-root {
      color-scheme: dark;
      --surface-1:      #1a1a19;
      --page:           #0d0d0d;
      --text-primary:   #ffffff;
      --text-secondary: #c3c2b7;
      --text-muted:     #898781;
      --grid:           #2c2c2a;
      --baseline:       #383835;
      --border:         rgba(255,255,255,0.10);
      --hover:          rgba(255,255,255,0.06);
__DARK_VARS__
    }
  }
  :root[data-theme="dark"] .viz-root {
    color-scheme: dark;
    --surface-1:      #1a1a19;
    --page:           #0d0d0d;
    --text-primary:   #ffffff;
    --text-secondary: #c3c2b7;
    --text-muted:     #898781;
    --grid:           #2c2c2a;
    --baseline:       #383835;
    --border:         rgba(255,255,255,0.10);
    --hover:          rgba(255,255,255,0.06);
__DARK_VARS__
  }

  * { box-sizing: border-box; }

  .card {
    max-width: 1000px;
    margin: 0 auto;
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 28px 28px 20px;
  }
  h1 { font-size: 17px; font-weight: 600; margin: 0 0 2px; }
  .sub { font-size: 13px; color: var(--text-secondary); margin: 0 0 4px; }
  .hint { font-size: 12px; color: var(--text-muted); margin: 0 0 16px; }

  .legend { display: flex; gap: 6px 16px; flex-wrap: wrap; margin-bottom: 10px; }
  .legend-item {
    display: flex; align-items: center; gap: 6px;
    font-size: 12.5px; color: var(--text-secondary);
    background: none; border: 0; padding: 2px 6px; border-radius: 6px;
    cursor: pointer; font-family: inherit;
  }
  .legend-item:hover { background: var(--hover); }
  .legend-item:focus-visible { outline: 2px solid var(--series-blue); outline-offset: 1px; }
  .legend-item[aria-pressed="false"] { opacity: 0.34; }
  .swatch { width: 20px; height: 0; border-top-style: solid; flex: none; }

  .chart-wrap { position: relative; }
  svg { display: block; width: 100%; height: auto; overflow: visible; }
  .gridline { stroke: var(--grid); stroke-width: 1; }
  .baseline { stroke: var(--baseline); stroke-width: 1; }
  .axis-label { fill: var(--text-muted); font-size: 11px; }
  .panel-title { fill: var(--text-secondary); font-size: 12px; font-weight: 600; }
  .panel-sub { fill: var(--text-muted); font-size: 10.5px; }
  .series-line { fill: none; stroke-linejoin: round; stroke-linecap: round; }
  .end-label { font-size: 10.5px; font-weight: 600; }
  .crosshair { stroke: var(--text-muted); stroke-width: 1; stroke-dasharray: 2 3; opacity: 0; pointer-events: none; }
  .hover-dot { opacity: 0; pointer-events: none; }
  .hover-target { fill: transparent; }
  .dimmed { opacity: 0.12; }

  .tooltip {
    position: absolute; pointer-events: none;
    background: var(--surface-1); border: 1px solid var(--border);
    border-radius: 8px; padding: 8px 10px; font-size: 12px;
    box-shadow: 0 4px 16px rgba(0,0,0,0.18);
    opacity: 0; transition: opacity 0.08s ease; min-width: 210px; z-index: 3;
  }
  .tooltip-title { font-weight: 600; margin-bottom: 5px; color: var(--text-primary); }
  .tooltip-row { display: flex; justify-content: space-between; gap: 14px; align-items: center; padding: 1px 0; color: var(--text-secondary); }
  .tooltip-row .name { display: flex; align-items: center; gap: 6px; }
  .tooltip-row .val { color: var(--text-primary); font-variant-numeric: tabular-nums; font-weight: 600; }
  .tt-swatch { width: 12px; height: 0; border-top-style: solid; border-top-width: 2.5px; flex: none; }

  details.table-view { margin-top: 16px; }
  details.table-view summary { font-size: 12.5px; color: var(--text-secondary); cursor: pointer; }
  .table-scroll { overflow-x: auto; margin-top: 10px; }
  table { border-collapse: collapse; font-size: 12px; min-width: 100%; }
  th, td { padding: 4px 10px; text-align: right; white-space: nowrap; border-bottom: 1px solid var(--border); }
  th:first-child, td:first-child { text-align: left; }
  thead th { color: var(--text-secondary); font-weight: 600; }
  tbody td { color: var(--text-primary); font-variant-numeric: tabular-nums; }
  caption { text-align: left; font-size: 12px; color: var(--text-muted); padding-bottom: 6px; }

  .footnote { font-size: 11.5px; color: var(--text-muted); margin-top: 14px; line-height: 1.55; }
</style>

<div class="viz-root" id="root-chart">
  <div class="card">
    <h1>__TITLE__</h1>
    <p class="sub">__SUBTITLE__</p>
    <p class="hint">Hover for every protocol's value at that balloon count. Click a legend entry to isolate it; click again to restore.</p>
    <div class="legend" id="legend"></div>
    <div class="chart-wrap">
      <svg id="chart" viewBox="0 0 900 620" preserveAspectRatio="xMidYMid meet" role="img" aria-label="__TITLE__"></svg>
      <div class="tooltip" id="tooltip" role="status"></div>
    </div>
    <details class="table-view">
      <summary>Show the numbers</summary>
      <div class="table-scroll" id="tables"></div>
    </details>
__FOOTNOTE__
  </div>
</div>

<script>
(function () {
  const NS = "http://www.w3.org/2000/svg";
  const meta = __META__;
  const data = __DATA__;
  const panels = __PANELS__;
  const xs = __XS__;

  const root = document.getElementById("root-chart");
  const cs = getComputedStyle(root);
  const color = (m) => cs.getPropertyValue("--series-" + m.hue).trim();

  const W = 900, H = 620;
  const padL = 62, padR = 78, padT = 26, padB = 40;
  const gap = 46;
  const plotW = W - padL - padR;
  const plotH = (H - padT - padB - gap) / 2;
  const panelTop = [padT, padT + plotH + gap];

  // Log x: the sweep's balloon counts span 100..3000 and are spaced by ratio,
  // so a linear axis would crush everything below n=1200 into the left margin.
  const lx = xs.map((n) => Math.log10(n));
  const xMin = Math.min(...lx), xMax = Math.max(...lx);
  const xPos = (i) => padL + plotW * (lx[i] - xMin) / (xMax - xMin);
  const yPos = (p, v) => panelTop[p] + plotH * (1 - v / 100);

  const svg = document.getElementById("chart");
  const el = (tag, attrs, text) => {
    const e = document.createElementNS(NS, tag);
    for (const k in attrs) if (attrs[k] !== null) e.setAttribute(k, attrs[k]);
    if (text !== undefined) e.textContent = text;
    return e;
  };

  // --- axes ---------------------------------------------------------------
  panels.forEach((p, pi) => {
    for (let v = 0; v <= 100; v += 25) {
      const y = yPos(pi, v);
      svg.appendChild(el("line", { x1: padL, x2: W - padR, y1: y, y2: y,
        class: v === 0 ? "baseline" : "gridline" }));
      svg.appendChild(el("text", { x: padL - 8, y: y + 4, class: "axis-label", "text-anchor": "end" }, v + "%"));
    }
    const head = el("text", { x: padL, y: panelTop[pi] - 12, class: "panel-title" }, p.title);
    head.appendChild(el("tspan", { class: "panel-sub", dx: 8 }, p.sub));
    svg.appendChild(head);
  });
  xs.forEach((n, i) => {
    svg.appendChild(el("text", { x: xPos(i), y: panelTop[1] + plotH + 18, class: "axis-label",
      "text-anchor": "middle" }, n));
  });
  svg.appendChild(el("text", { x: padL + plotW / 2, y: H - 4, class: "axis-label",
    "text-anchor": "middle" }, "Balloon count (n), log scale"));

  // --- series -------------------------------------------------------------
  const groups = {};   // key -> [<g> per panel]
  const endLabels = panels.map(() => []);
  meta.forEach((m) => {
    groups[m.key] = panels.map((p, pi) => {
      const g = el("g", { "data-key": m.key });
      const s = data[m.key][p.metric];
      const stroke = color(m);

      if (m.band) {
        // +/-1 SEM. Drawn only for the headline lines and ground truth: eleven
        // overlapping bands would be mud, and the thin dashed variants are
        // there to show a family's shape, not a precise value.
        let seg = [];
        const flushBand = () => {
          if (seg.length < 2) { seg = []; return; }
          const up = seg.map((i) => `${xPos(i)},${yPos(pi, s.hi[i])}`);
          const dn = seg.slice().reverse().map((i) => `${xPos(i)},${yPos(pi, s.lo[i])}`);
          g.appendChild(el("polygon", { points: up.concat(dn).join(" "), fill: stroke,
            "fill-opacity": 0.15, stroke: "none" }));
          seg = [];
        };
        s.mean.forEach((v, i) => { if (v === null) flushBand(); else seg.push(i); });
        flushBand();
      }

      // Break the line across gaps rather than interpolating over them.
      let seg = [];
      const flush = () => {
        if (seg.length === 0) { return; }
        if (seg.length > 1) {
          g.appendChild(el("polyline", {
            points: seg.map((i) => `${xPos(i)},${yPos(pi, s.mean[i])}`).join(" "),
            class: "series-line", stroke: stroke,
            "stroke-width": m.headline ? 2.4 : 1.3,
            "stroke-opacity": m.headline ? 1 : 0.6,
            "stroke-dasharray": m.dash,
          }));
        }
        if (m.headline) {
          seg.forEach((i) => g.appendChild(el("circle", { cx: xPos(i), cy: yPos(pi, s.mean[i]),
            r: 3.4, fill: stroke, stroke: cs.getPropertyValue("--surface-1").trim(), "stroke-width": 1.5 })));
        }
        seg = [];
      };
      s.mean.forEach((v, i) => { if (v === null) flush(); else seg.push(i); });
      flush();

      // Direct label on the headline lines: the light palette's aqua and
      // yellow sit under 3:1 on the light surface, and the documented relief
      // for that is a visible label rather than color alone. Placement is
      // deferred to placeEndLabels() so lines that finish close together
      // don't stack their labels on top of each other.
      if (m.headline) {
        let last = s.mean.length - 1;
        while (last >= 0 && s.mean[last] === null) last--;
        if (last >= 0) {
          const t = el("text", { x: xPos(last) + 8, y: yPos(pi, s.mean[last]) + 3.5,
            class: "end-label", fill: stroke }, m.end);
          g.appendChild(t);
          endLabels[pi].push({ node: t, want: yPos(pi, s.mean[last]) + 3.5 });
        }
      }
      svg.appendChild(g);
      return g;
    });
  });

  // Nudge overlapping end labels apart, keeping them inside their panel and
  // as close to their line's last point as the 13px minimum gap allows.
  function placeEndLabels() {
    endLabels.forEach((labels, pi) => {
      const MIN = 13;
      const top = panelTop[pi] - 4, bot = panelTop[pi] + plotH + 4;
      labels.sort((a, b) => a.want - b.want);
      let y = top;
      labels.forEach((l) => { y = l.y = Math.max(l.want, y); y += MIN; });
      // If the pile ran past the bottom, push the whole stack back up.
      const over = labels.length ? labels[labels.length - 1].y - bot : 0;
      if (over > 0) {
        let limit = bot;
        for (let i = labels.length - 1; i >= 0; i--) {
          labels[i].y = Math.min(labels[i].y, limit);
          limit = labels[i].y - MIN;
        }
      }
      labels.forEach((l) => l.node.setAttribute("y", l.y));
    });
  }
  placeEndLabels();

  // --- legend -------------------------------------------------------------
  let isolated = null;
  const legend = document.getElementById("legend");
  meta.forEach((m) => {
    const b = document.createElement("button");
    b.className = "legend-item";
    b.type = "button";
    b.setAttribute("aria-pressed", "true");
    b.dataset.key = m.key;
    b.innerHTML = `<span class="swatch" style="border-top-color:${color(m)};`
      + `border-top-width:${m.headline ? 2.5 : 1.5}px;`
      + `border-top-style:${m.dash ? "dashed" : "solid"}"></span>${m.label}`;
    b.addEventListener("click", () => {
      isolated = isolated === m.key ? null : m.key;
      applyIsolation();
    });
    legend.appendChild(b);
  });
  function applyIsolation() {
    meta.forEach((m) => {
      const on = isolated === null || isolated === m.key;
      groups[m.key].forEach((g) => g.classList.toggle("dimmed", !on));
      legend.querySelector(`[data-key="${m.key}"]`).setAttribute("aria-pressed", on ? "true" : "false");
    });
  }

  // --- hover --------------------------------------------------------------
  const crosshairs = panels.map((_, pi) => {
    const c = el("line", { x1: 0, x2: 0, y1: panelTop[pi], y2: panelTop[pi] + plotH, class: "crosshair" });
    svg.appendChild(c);
    return c;
  });
  const hoverDots = {};
  meta.forEach((m) => {
    hoverDots[m.key] = panels.map(() => {
      const c = el("circle", { r: 5, class: "hover-dot", fill: color(m),
        stroke: cs.getPropertyValue("--surface-1").trim(), "stroke-width": 2 });
      svg.appendChild(c);
      return c;
    });
  });

  const tooltip = document.getElementById("tooltip");
  const wrap = svg.closest(".chart-wrap");
  const mid = [];
  for (let i = 0; i < xs.length; i++) {
    const a = i === 0 ? padL : (xPos(i - 1) + xPos(i)) / 2;
    const b = i === xs.length - 1 ? padL + plotW : (xPos(i) + xPos(i + 1)) / 2;
    mid.push([a, b]);
  }
  panels.forEach((p, pi) => {
    xs.forEach((_, i) => {
      const [a, b] = mid[i];
      const t = el("rect", { x: a, y: panelTop[pi], width: b - a, height: plotH, class: "hover-target" });
      t.addEventListener("mouseenter", () => show(pi, i));
      t.addEventListener("mousemove", (ev) => place(ev));
      t.addEventListener("mouseleave", hide);
      svg.appendChild(t);
    });
  });

  function show(pi, i) {
    crosshairs.forEach((c, k) => {
      c.setAttribute("x1", xPos(i)); c.setAttribute("x2", xPos(i));
      c.style.opacity = k === pi ? 1 : 0.45;
    });
    const visible = meta.filter((m) => isolated === null || isolated === m.key);
    meta.forEach((m) => {
      const v = data[m.key][panels[pi].metric].mean[i];
      hoverDots[m.key].forEach((d, k) => {
        const on = k === pi && v !== null && (isolated === null || isolated === m.key);
        d.style.opacity = on ? 1 : 0;
        if (on) { d.setAttribute("cx", xPos(i)); d.setAttribute("cy", yPos(pi, v)); }
      });
    });
    // Ranked, because "who is winning at this density" is the question this
    // chart exists to answer, and legend order can't answer it per-column.
    const rows = visible
      .map((m) => ({ m, v: data[m.key][panels[pi].metric].mean[i] }))
      .sort((x, y) => (y.v === null ? -1 : x.v === null ? 1 : y.v - x.v))
      .map(({ m, v }) => `<div class="tooltip-row"><span class="name">`
        + `<span class="tt-swatch" style="border-top-color:${color(m)};border-top-style:${m.dash ? "dashed" : "solid"}"></span>`
        + `${m.label}</span><span class="val">${v === null ? "—" : v.toFixed(1) + "%"}</span></div>`)
      .join("");
    tooltip.innerHTML = `<div class="tooltip-title">n = ${xs[i]} · ${panels[pi].title}</div>${rows}`;
    tooltip.style.opacity = 1;
  }
  function place(ev) {
    const r = wrap.getBoundingClientRect();
    const x = ev.clientX - r.left, y = ev.clientY - r.top;
    const flip = x > r.width * 0.55;
    tooltip.style.left = Math.max(0, flip ? x - tooltip.offsetWidth - 16 : x + 16) + "px";
    tooltip.style.top = Math.min(Math.max(0, y - 40), r.height - tooltip.offsetHeight) + "px";
  }
  function hide() {
    crosshairs.forEach((c) => (c.style.opacity = 0));
    Object.values(hoverDots).forEach((ds) => ds.forEach((d) => (d.style.opacity = 0)));
    tooltip.style.opacity = 0;
  }

  // --- table view ---------------------------------------------------------
  const tables = document.getElementById("tables");
  panels.forEach((p) => {
    const t = document.createElement("table");
    t.innerHTML = `<caption>${p.title} — ${p.sub}, mean over seeds</caption>`
      + `<thead><tr><th>Protocol</th>${xs.map((n) => `<th>n=${n}</th>`).join("")}</tr></thead>`
      + `<tbody>${meta.map((m) => `<tr><th scope="row">${m.label}</th>`
        + data[m.key][p.metric].mean.map((v) => `<td>${v === null ? "—" : v.toFixed(1)}</td>`).join("")
        + `</tr>`).join("")}</tbody>`;
    tables.appendChild(t);
  });
})();
</script>
"""


def render(out_path, title, subtitle, footnote, xs, meta, data):
    light_vars = "\n".join(f"    --series-{k}: {v};" for k, v in LIGHT.items())
    dark_vars = "\n".join(f"    --series-{k}: {v};" for k, v in DARK.items())
    panels = [{"metric": key, "title": t.replace(" (%)", ""), "sub": sub} for key, t, sub in PANELS]

    html = TEMPLATE
    for token, value in [
        ("__TITLE__", title),
        ("__SUBTITLE__", subtitle),
        ("__FOOTNOTE__", f'    <p class="footnote">{footnote}</p>\n' if footnote else ""),
        ("__LIGHT_VARS__", light_vars),
        ("__DARK_VARS__", dark_vars),
        ("__META__", json.dumps(meta)),
        ("__DATA__", json.dumps(data)),
        ("__PANELS__", json.dumps(panels)),
        ("__XS__", json.dumps(xs)),
    ]:
        html = html.replace(token, value)

    with open(out_path, "w") as f:
        f.write(html)
    print(f"Wrote {out_path}")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv_path")
    ap.add_argument("--out", required=True, help="output HTML path")
    ap.add_argument("--truth", help="ground_truth_sweep.rs CSV (n,seed,meanDegree,groundedPct) to overlay")
    ap.add_argument("--title", default="Protocol density sweep: does the ranking hold as n changes?")
    ap.add_argument("--subtitle",
                    default="dv-dtn variants, reactive/link-state discovery, spray-and-wait — zero wind, density_sweep.rs")
    ap.add_argument("--footnote",
                    default="Bands are ±1 standard error over seeds, drawn for the headline contenders and the "
                            "ground-truth ceiling only. Ground truth is the share of balloons whose physical "
                            "connectivity component contains a tower (union-find, computed before any protocol is "
                            "consulted) — no protocol can exceed it.")
    args = ap.parse_args()

    data = load(args.csv_path)
    truth = load_truth(args.truth) if args.truth else None

    ns = sorted({n for label in data for n in data[label]})
    meta, series = build_series(data, truth, ns)
    render(args.out, args.title, args.subtitle, args.footnote, ns, meta, series)


if __name__ == "__main__":
    main()
