#!/usr/bin/env python3
"""Interactive HTML twin of plot_protocol_sweep.py's PNG — same CSV, same five
series, plotted against mean node degree, with a crosshair, per-point tooltips
and a data table.

The gain over the static figure is specific. Each point is one
(horizonCoeff, nBalloons) combo collapsed onto the degree axis, and that
collapse is the chart's whole argument (docs/design/MESH_COMMS_DESIGN.md §1.1:
sweeps over balloon count and horizon coefficient land on the same curve when
read by degree). The PNG can show the collapse but cannot say *which* combo any
point came from, so the non-monotonic stretch between degree 3 and 6 — where a
sparse-but-large field and a dense-but-small one interleave — reads as noise.
Hovering names the combo, and the wobble becomes legible.

Color departs from the PNG in exactly one place, deliberately. The PNG draws
"delivered" in pink #e87ba4 and "delivered without ack" in red #e05561, a pair
that fails the normal-vision separation floor outright (ΔE 10.4, floor 15) —
they are hard to tell apart with full color vision, let alone without, and they
are the two lines a reader most wants to compare. The unacked series moves to
violet here (dataviz slot 7, validated in both modes) and keeps that hue in
light and dark. Everything else is the app's own belief-vs-truth vocabulary
from src/main.js BELIEF_COLORS, unchanged.

One documented exception: the ground-truth green is the app's #5fd08a, whose
OKLCH lightness (0.774) overshoots the light band's 0.77 ceiling by 0.004. It
is kept rather than re-stepped so this chart, the density chart and the running
app all name ground truth with the same green; the miss is not visible and the
series carries a direct label besides.

Usage:
    python3 experiments/plot_protocol_sweep_html.py experiments/protocol-sweep-results.csv \
        --out experiments/protocol-results/truth-vs-belief-vs-delivery.html
"""

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from plot_protocol_sweep import load_rows  # noqa: E402

# Percolation threshold, same convention as the Controls panel's mesh-health
# readout in the running app.
PERCOLATION_DEGREE = 4.5

LIGHT = {
    "truth": "#5fd08a",     # "ok" green — src/main.js BELIEF_COLORS
    "belief": "#e0a355",    # "unaware" amber — same source
    "achieved": "#2a78d6",  # dataviz slot 1
    "delivered": "#e87ba4",  # dataviz slot 5
    "unacked": "#4a3aa7",   # dataviz slot 7 (replaces the PNG's #e05561)
}
DARK = {
    "truth": "#35a96c",
    "belief": "#c98500",
    "achieved": "#3987e5",
    "delivered": "#d55181",
    "unacked": "#9085e9",
}

# (hue key, legend label, end label, CSV-derived field)
SERIES = [
    ("truth", "Ground truth: actually reachable (union-find)", "truth", "groundedPct"),
    ("belief", "Belief: balloons that think they have a route", "belief", "believedGroundedPct"),
    ("achieved", "Achieved: completion rate (delivered/resolved)", "completion", "completionRate100"),
    ("delivered", "Bundles delivered, % of originated", "delivered", "deliveredPctOfOriginated"),
    ("unacked", "Delivered but never acked, % of originated", "unacked", "unackedPctOfOriginated"),
]


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
    max-width: 940px; margin: 0 auto;
    background: var(--surface-1); border: 1px solid var(--border);
    border-radius: 12px; padding: 28px 28px 20px;
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
  .legend-item:focus-visible { outline: 2px solid var(--series-achieved); outline-offset: 1px; }
  .legend-item[aria-pressed="false"] { opacity: 0.34; }
  .swatch { width: 20px; height: 0; border-top: 2.5px solid; flex: none; }

  .chart-wrap { position: relative; }
  svg { display: block; width: 100%; height: auto; overflow: visible; }
  .gridline { stroke: var(--grid); stroke-width: 1; }
  .baseline { stroke: var(--baseline); stroke-width: 1; }
  .axis-label { fill: var(--text-muted); font-size: 11px; }
  .marker-line { stroke: var(--baseline); stroke-width: 1; stroke-dasharray: 4 4; }
  .marker-text { fill: var(--text-muted); font-size: 10.5px; }
  .series-line { fill: none; stroke-width: 2.2; stroke-linejoin: round; stroke-linecap: round; }
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
    opacity: 0; transition: opacity 0.08s ease; min-width: 250px; z-index: 3;
  }
  .tooltip-title { font-weight: 600; margin-bottom: 1px; color: var(--text-primary); }
  .tooltip-combo { color: var(--text-muted); margin-bottom: 5px; font-size: 11.5px; }
  .tooltip-row { display: flex; justify-content: space-between; gap: 14px; align-items: center; padding: 1px 0; color: var(--text-secondary); }
  .tooltip-row .name { display: flex; align-items: center; gap: 6px; }
  .tooltip-row .val { color: var(--text-primary); font-variant-numeric: tabular-nums; font-weight: 600; }
  .tt-swatch { width: 12px; height: 0; border-top: 2.5px solid; flex: none; }

  details.table-view { margin-top: 16px; }
  details.table-view summary { font-size: 12.5px; color: var(--text-secondary); cursor: pointer; }
  .table-scroll { overflow-x: auto; margin-top: 10px; }
  table { border-collapse: collapse; font-size: 12px; min-width: 100%; }
  th, td { padding: 4px 10px; text-align: right; white-space: nowrap; border-bottom: 1px solid var(--border); }
  th:first-child, td:first-child { text-align: left; }
  thead th { color: var(--text-secondary); font-weight: 600; }
  tbody td { color: var(--text-primary); font-variant-numeric: tabular-nums; }

  .footnote { font-size: 11.5px; color: var(--text-muted); margin-top: 14px; line-height: 1.55; }
</style>

<div class="viz-root" id="root-chart">
  <div class="card">
    <h1>__TITLE__</h1>
    <p class="sub">__SUBTITLE__</p>
    <p class="hint">Hover a point for its values and the (horizon coefficient, balloon count) combo it came from. Click a legend entry to isolate that series.</p>
    <div class="legend" id="legend"></div>
    <div class="chart-wrap">
      <svg id="chart" viewBox="0 0 900 440" preserveAspectRatio="xMidYMid meet" role="img" aria-label="__TITLE__"></svg>
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
  const series = __SERIES__;
  const points = __POINTS__;
  const marker = __MARKER__;

  const root = document.getElementById("root-chart");
  const cs = getComputedStyle(root);
  const color = (s) => cs.getPropertyValue("--series-" + s.hue).trim();
  const surface = () => cs.getPropertyValue("--surface-1").trim();

  const W = 900, H = 440;
  const padL = 52, padR = 92, padT = 34, padB = 46;
  const plotW = W - padL - padR, plotH = H - padT - padB;

  // Linear degree axis, not one tick per row: the point of this chart is that
  // combos land where their *degree* puts them, so the spacing has to be
  // proportional or the collapse it argues for isn't visible.
  const xs = points.map((p) => p.meanDegree);
  const xMax = Math.ceil(Math.max(...xs) / 2) * 2;
  const xPos = (d) => padL + plotW * d / xMax;
  const yPos = (v) => padT + plotH * (1 - v / 100);

  const svg = document.getElementById("chart");
  const el = (tag, attrs, text) => {
    const e = document.createElementNS(NS, tag);
    for (const k in attrs) if (attrs[k] !== null) e.setAttribute(k, attrs[k]);
    if (text !== undefined) e.textContent = text;
    return e;
  };

  for (let v = 0; v <= 100; v += 25) {
    const y = yPos(v);
    svg.appendChild(el("line", { x1: padL, x2: W - padR, y1: y, y2: y,
      class: v === 0 ? "baseline" : "gridline" }));
    svg.appendChild(el("text", { x: padL - 8, y: y + 4, class: "axis-label", "text-anchor": "end" }, v + "%"));
  }
  for (let d = 0; d <= xMax; d += 2) {
    svg.appendChild(el("text", { x: xPos(d), y: H - padB + 18, class: "axis-label",
      "text-anchor": "middle" }, d));
  }
  svg.appendChild(el("text", { x: padL + plotW / 2, y: H - 6, class: "axis-label",
    "text-anchor": "middle" }, "Mean node degree"));

  svg.appendChild(el("line", { x1: xPos(marker.at), x2: xPos(marker.at), y1: padT - 4,
    y2: padT + plotH, class: "marker-line" }));
  marker.label.forEach((line, i) => {
    svg.appendChild(el("text", { x: xPos(marker.at), y: padT - 18 + i * 12,
      class: "marker-text", "text-anchor": "middle" }, line));
  });

  const groups = {};
  const endLabels = [];
  series.forEach((s) => {
    const g = el("g", { "data-key": s.hue });
    const stroke = color(s);
    g.appendChild(el("polyline", {
      points: points.map((p) => `${xPos(p.meanDegree)},${yPos(p[s.field])}`).join(" "),
      class: "series-line", stroke: stroke,
    }));
    points.forEach((p) => g.appendChild(el("circle", {
      cx: xPos(p.meanDegree), cy: yPos(p[s.field]), r: 3.6, fill: stroke,
      stroke: surface(), "stroke-width": 1.5,
    })));
    // Direct labels: three of the light steps sit under 3:1 on the light
    // surface, whose documented relief is a visible label or a table view.
    // Both ship; this is the label half.
    const last = points[points.length - 1];
    const t = el("text", { x: xPos(last.meanDegree) + 8, y: yPos(last[s.field]) + 3.5,
      class: "end-label", fill: stroke }, s.end);
    g.appendChild(t);
    endLabels.push({ node: t, want: yPos(last[s.field]) + 3.5 });
    svg.appendChild(g);
    groups[s.hue] = g;
  });

  // Keep end labels from stacking where two series finish close together.
  (function placeEndLabels() {
    const MIN = 13;
    endLabels.sort((a, b) => a.want - b.want);
    let y = padT - 4;
    endLabels.forEach((l) => { y = l.y = Math.max(l.want, y); y += MIN; });
    let limit = padT + plotH + 4;
    for (let i = endLabels.length - 1; i >= 0; i--) {
      endLabels[i].y = Math.min(endLabels[i].y, limit);
      limit = endLabels[i].y - MIN;
    }
    endLabels.forEach((l) => l.node.setAttribute("y", l.y));
  })();

  let isolated = null;
  const legend = document.getElementById("legend");
  series.forEach((s) => {
    const b = document.createElement("button");
    b.className = "legend-item";
    b.type = "button";
    b.setAttribute("aria-pressed", "true");
    b.dataset.key = s.hue;
    b.innerHTML = `<span class="swatch" style="border-top-color:${color(s)}"></span>${s.label}`;
    b.addEventListener("click", () => {
      isolated = isolated === s.hue ? null : s.hue;
      series.forEach((o) => {
        const on = isolated === null || isolated === o.hue;
        groups[o.hue].classList.toggle("dimmed", !on);
        legend.querySelector(`[data-key="${o.hue}"]`).setAttribute("aria-pressed", on ? "true" : "false");
      });
    });
    legend.appendChild(b);
  });

  const crosshair = el("line", { x1: 0, x2: 0, y1: padT, y2: padT + plotH, class: "crosshair" });
  svg.appendChild(crosshair);
  const hoverDots = series.map((s) => {
    const c = el("circle", { r: 5.5, class: "hover-dot", fill: color(s), stroke: surface(), "stroke-width": 2 });
    svg.appendChild(c);
    return c;
  });

  const tooltip = document.getElementById("tooltip");
  const wrap = svg.closest(".chart-wrap");
  points.forEach((p, i) => {
    const a = i === 0 ? padL : (xPos(points[i - 1].meanDegree) + xPos(p.meanDegree)) / 2;
    const b = i === points.length - 1 ? padL + plotW
      : (xPos(p.meanDegree) + xPos(points[i + 1].meanDegree)) / 2;
    const t = el("rect", { x: a, y: padT, width: Math.max(1, b - a), height: plotH, class: "hover-target" });
    t.addEventListener("mouseenter", () => show(i));
    t.addEventListener("mousemove", place);
    t.addEventListener("mouseleave", hide);
    svg.appendChild(t);
  });

  function show(i) {
    const p = points[i];
    crosshair.setAttribute("x1", xPos(p.meanDegree));
    crosshair.setAttribute("x2", xPos(p.meanDegree));
    crosshair.style.opacity = 1;
    series.forEach((s, k) => {
      const on = isolated === null || isolated === s.hue;
      hoverDots[k].style.opacity = on ? 1 : 0;
      hoverDots[k].setAttribute("cx", xPos(p.meanDegree));
      hoverDots[k].setAttribute("cy", yPos(p[s.field]));
    });
    const rows = series
      .filter((s) => isolated === null || isolated === s.hue)
      .map((s) => `<div class="tooltip-row"><span class="name">`
        + `<span class="tt-swatch" style="border-top-color:${color(s)}"></span>${s.label}</span>`
        + `<span class="val">${p[s.field].toFixed(1)}%</span></div>`)
      .join("");
    tooltip.innerHTML = `<div class="tooltip-title">Mean degree ${p.meanDegree.toFixed(2)}</div>`
      + `<div class="tooltip-combo">horizon coeff ${p.horizonCoeff} · n = ${p.nBalloons} · `
      + `${p.originated.toLocaleString()} bundles originated</div>${rows}`;
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
    crosshair.style.opacity = 0;
    hoverDots.forEach((d) => (d.style.opacity = 0));
    tooltip.style.opacity = 0;
  }

  const table = document.createElement("table");
  table.innerHTML = `<thead><tr><th>Horizon coeff</th><th>n</th><th>Mean degree</th>`
    + series.map((s) => `<th>${s.end}</th>`).join("") + `</tr></thead><tbody>`
    + points.map((p) => `<tr><td>${p.horizonCoeff}</td><td>${p.nBalloons}</td>`
      + `<td>${p.meanDegree.toFixed(2)}</td>`
      + series.map((s) => `<td>${p[s.field].toFixed(1)}</td>`).join("") + `</tr>`).join("")
    + `</tbody>`;
  document.getElementById("tables").appendChild(table);
})();
</script>
"""


def render(out_path, title, subtitle, footnote, rows):
    points = []
    for r in rows:
        points.append({
            "horizonCoeff": r["horizonCoeff"],
            "nBalloons": int(r["nBalloons"]),
            "meanDegree": round(r["meanDegree"], 4),
            "originated": int(r["originated"]),
            "groundedPct": round(r["groundedPct"], 4),
            "believedGroundedPct": round(r["believedGroundedPct"], 4),
            "completionRate100": round(100.0 * r["completionRate"], 4),
            "deliveredPctOfOriginated": round(r["deliveredPctOfOriginated"], 4),
            "unackedPctOfOriginated": round(r["unackedPctOfOriginated"], 4),
        })

    meta = [{"hue": hue, "label": label, "end": end, "field": field}
            for hue, label, end, field in SERIES]

    html = TEMPLATE
    for token, value in [
        ("__TITLE__", title),
        ("__SUBTITLE__", subtitle),
        ("__FOOTNOTE__", f'    <p class="footnote">{footnote}</p>\n' if footnote else ""),
        ("__LIGHT_VARS__", "\n".join(f"    --series-{k}: {v};" for k, v in LIGHT.items())),
        ("__DARK_VARS__", "\n".join(f"    --series-{k}: {v};" for k, v in DARK.items())),
        ("__SERIES__", json.dumps(meta)),
        ("__POINTS__", json.dumps(points)),
        ("__MARKER__", json.dumps({"at": PERCOLATION_DEGREE, "label": ["percolation", "threshold"]})),
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
    ap.add_argument("--title", default="Truth vs. belief vs. real delivery, by mesh density")
    ap.add_argument("--subtitle", default="C1+C2 decentralized protocol, real wind — protocol_sweep.rs")
    ap.add_argument("--footnote",
                    default="Completion rate is delivered/resolved; delivered is delivered/originated. The gap "
                            "between them is the censoring bias from bundles still legitimately in flight at the "
                            "cutoff, which is why both are shown. Points are the sweep's own (horizon coefficient, "
                            "balloon count) combos connected in degree order — not a fitted curve.")
    args = ap.parse_args()

    render(args.out, args.title, args.subtitle, args.footnote, load_rows(args.csv_path))


if __name__ == "__main__":
    main()
