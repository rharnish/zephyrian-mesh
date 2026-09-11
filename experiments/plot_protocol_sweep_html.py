#!/usr/bin/env python3
"""Interactive HTML twin of plot_protocol_sweep.py's PNG — same CSV, same
seed aggregation, same six series and the same three panels (one per balloon
count, mean node degree along x, horizon coefficient naming each point), with a
crosshair, per-point tooltips and a data table.

The gain over the static figure is specific. Each point is the mean of a
(horizonCoeff, nBalloons) cell's seeds, and the PNG can only show its ±1 sd as
a band. Hovering gives the exact mean ± sd for every series, the seed count and
the bundles originated, so a reader can tell a real gap between two series
from overlapping spread.

Colours are the PNG's, light and dark, and were validated as a set in this
series order (dataviz validate_palette.js, adjacent pairs): every pair clears
the normal-vision floor in both modes, and the dark steps clear CVD ΔE 8. Two
documented exceptions, both kept so the charts and the running app name ground
truth and belief identically: the ground-truth green #5fd08a's OKLCH lightness
(0.774) overshoots the light band's 0.77 ceiling by 0.004, and in light mode
truth↔belief is CVD ΔE 6.2, legal only with secondary encoding — which the
tooltip's named rows and the table provide. Three light steps sit under 3:1 on
the light surface; the table view is the relief for that.

Usage:
    python3 experiments/plot_protocol_sweep_html.py experiments/protocol-sweep-results.csv \
        --out experiments/protocol-results/truth-vs-belief-vs-delivery.html
"""

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from plot_protocol_sweep import PERCOLATION_DEGREE, SERIES, load_cells  # noqa: E402

LIGHT = {
    "truth": "#5fd08a",       # "ok" green — src/overlays.js BELIEF_CSS
    "belief": "#e0a355",      # "unaware" amber — same source
    "completion": "#2a78d6",  # dataviz slot 1
    "delivered": "#e87ba4",   # slot 5
    "acked": "#008300",       # slot 6
    "unacked": "#4a3aa7",     # slot 7
}
DARK = {
    "truth": "#35a96c",
    "belief": "#c98500",
    "completion": "#3987e5",
    "delivered": "#d55181",
    "acked": "#008300",
    "unacked": "#9085e9",
}

# Short names for the tooltip and table headers, keyed like SERIES.
SHORT = {
    "truth": "truth",
    "belief": "belief",
    "completion": "completion",
    "delivered": "reached tower",
    "acked": "acked",
    "unacked": "ack lost",
}


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
  .series-line { fill: none; stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }
  .band { opacity: 0.16; stroke: none; }
  .panel-title { fill: var(--text-primary); font-size: 12.5px; font-weight: 600; }
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
    <p class="hint">One panel per balloon count; the grey numbers along the top are each point's horizon coefficient. Shaded bands are ±1 sd across seeds. Hover a point for mean ± sd; click a legend entry to isolate that series.</p>
    <div class="legend" id="legend"></div>
    <div class="chart-wrap">
      <svg id="chart" viewBox="0 0 900 400" preserveAspectRatio="xMidYMid meet" role="img" aria-label="__TITLE__"></svg>
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
  const cells = __CELLS__;
  const marker = __MARKER__;

  const root = document.getElementById("root-chart");
  const cs = getComputedStyle(root);
  const color = (s) => cs.getPropertyValue("--series-" + s.key).trim();
  const surface = () => cs.getPropertyValue("--surface-1").trim();

  const W = 900, H = 400;
  const padL = 44, padR = 12, padT = 58, padB = 40, gap = 26;
  const ns = [...new Set(cells.map((c) => c.n))].sort((a, b) => a - b);
  const panelW = (W - padL - padR - gap * (ns.length - 1)) / ns.length;
  const plotH = H - padT - padB;
  const yPos = (v) => padT + plotH * (1 - Math.max(-3, Math.min(103, v)) / 100);

  const svg = document.getElementById("chart");
  const el = (tag, attrs, text) => {
    const e = document.createElementNS(NS, tag);
    for (const k in attrs) if (attrs[k] !== null) e.setAttribute(k, attrs[k]);
    if (text !== undefined) e.textContent = text;
    return e;
  };

  const groups = {};
  series.forEach((s) => { groups[s.key] = []; });
  const panels = ns.map((n, pi) => {
    const x0 = padL + pi * (panelW + gap);
    const pts = cells.filter((c) => c.n === n).sort((a, b) => a.degree - b.degree);
    const lo = Math.min(...pts.map((c) => c.degree)), hi = Math.max(...pts.map((c) => c.degree));
    const pad = 0.06 * (hi - lo);
    const xPos = (d) => x0 + panelW * (d - (lo - pad)) / (hi - lo + 2 * pad);

    svg.appendChild(el("text", { x: x0, y: 16, class: "panel-title" }, `${n} balloons`));
    for (let v = 0; v <= 100; v += 25) {
      svg.appendChild(el("line", { x1: x0, x2: x0 + panelW, y1: yPos(v), y2: yPos(v),
        class: v === 0 ? "baseline" : "gridline" }));
      if (pi === 0) {
        svg.appendChild(el("text", { x: padL - 8, y: yPos(v) + 4, class: "axis-label", "text-anchor": "end" }, v + "%"));
      }
    }
    // Degree ticks at a step that gives 3-5 labels whatever the panel's range.
    const step = [0.5, 1, 2, 5].find((st) => (hi - lo + 2 * pad) / st <= 5);
    for (let d = Math.ceil((lo - pad) / step) * step; d <= hi + pad; d += step) {
      svg.appendChild(el("text", { x: xPos(d), y: H - padB + 16, class: "axis-label", "text-anchor": "middle" },
        Number(d.toFixed(1))));
    }
    svg.appendChild(el("text", { x: x0 + panelW / 2, y: H - 6, class: "axis-label", "text-anchor": "middle" },
      "Mean node degree"));
    pts.forEach((c) => {
      svg.appendChild(el("text", { x: xPos(c.degree), y: padT - 10, class: "axis-label", "text-anchor": "middle" },
        c.horizon));
    });
    if (pi === 0) {
      svg.appendChild(el("text", { x: x0, y: padT - 26, class: "marker-text" }, "horizon coefficient"));
    }
    if (marker.at > lo - pad && marker.at < hi + pad) {
      svg.appendChild(el("line", { x1: xPos(marker.at), x2: xPos(marker.at), y1: padT, y2: padT + plotH,
        class: "marker-line" }));
      svg.appendChild(el("text", { x: xPos(marker.at) + 4, y: padT + plotH - 6, class: "marker-text" }, marker.label));
    }

    series.forEach((s) => {
      const g = el("g", { "data-key": s.key });
      const stroke = color(s);
      const upper = pts.map((c) => `${xPos(c.degree)},${yPos(c[s.key] + c[s.key + "_sd"])}`);
      const lower = pts.map((c) => `${xPos(c.degree)},${yPos(c[s.key] - c[s.key + "_sd"])}`).reverse();
      g.appendChild(el("polygon", { points: upper.concat(lower).join(" "), class: "band", fill: stroke }));
      g.appendChild(el("polyline", { points: pts.map((c) => `${xPos(c.degree)},${yPos(c[s.key])}`).join(" "),
        class: "series-line", stroke: stroke }));
      pts.forEach((c) => g.appendChild(el("circle", { cx: xPos(c.degree), cy: yPos(c[s.key]), r: 4,
        fill: stroke, stroke: surface(), "stroke-width": 2 })));
      svg.appendChild(g);
      groups[s.key].push(g);
    });
    return { x0, pts, xPos };
  });

  let isolated = null;
  const legend = document.getElementById("legend");
  series.forEach((s) => {
    const b = document.createElement("button");
    b.className = "legend-item";
    b.type = "button";
    b.setAttribute("aria-pressed", "true");
    b.dataset.key = s.key;
    b.innerHTML = `<span class="swatch" style="border-top-color:${color(s)}"></span>${s.label}`;
    b.addEventListener("click", () => {
      isolated = isolated === s.key ? null : s.key;
      series.forEach((o) => {
        const on = isolated === null || isolated === o.key;
        groups[o.key].forEach((g) => g.classList.toggle("dimmed", !on));
        legend.querySelector(`[data-key="${o.key}"]`).setAttribute("aria-pressed", on ? "true" : "false");
      });
    });
    legend.appendChild(b);
  });

  const crosshair = el("line", { y1: padT, y2: padT + plotH, class: "crosshair" });
  svg.appendChild(crosshair);
  const hoverDots = series.map((s) => {
    const c = el("circle", { r: 6, class: "hover-dot", fill: color(s), stroke: surface(), "stroke-width": 2 });
    svg.appendChild(c);
    return c;
  });

  const tooltip = document.getElementById("tooltip");
  const wrap = svg.closest(".chart-wrap");
  const fmt = (c, k) => `${c[k].toFixed(1)}% <span style="font-weight:400;color:var(--text-muted)">± ${c[k + "_sd"].toFixed(1)}</span>`;
  panels.forEach(({ x0, pts, xPos }) => {
    pts.forEach((c, i) => {
      const a = i === 0 ? x0 : (xPos(pts[i - 1].degree) + xPos(c.degree)) / 2;
      const b = i === pts.length - 1 ? x0 + panelW : (xPos(c.degree) + xPos(pts[i + 1].degree)) / 2;
      const t = el("rect", { x: a, y: padT, width: Math.max(1, b - a), height: plotH, class: "hover-target" });
      t.addEventListener("mouseenter", () => show(c, xPos));
      t.addEventListener("mousemove", place);
      t.addEventListener("mouseleave", hide);
      svg.appendChild(t);
    });
  });

  function show(c, xPos) {
    const x = xPos(c.degree);
    crosshair.setAttribute("x1", x);
    crosshair.setAttribute("x2", x);
    crosshair.style.opacity = 1;
    series.forEach((s, k) => {
      hoverDots[k].style.opacity = isolated === null || isolated === s.key ? 1 : 0;
      hoverDots[k].setAttribute("cx", x);
      hoverDots[k].setAttribute("cy", yPos(c[s.key]));
    });
    const rows = series
      .filter((s) => isolated === null || isolated === s.key)
      .map((s) => `<div class="tooltip-row"><span class="name">`
        + `<span class="tt-swatch" style="border-top-color:${color(s)}"></span>${s.short}</span>`
        + `<span class="val">${fmt(c, s.key)}</span></div>`)
      .join("");
    tooltip.innerHTML = `<div class="tooltip-title">${c.n} balloons · horizon ${c.horizon}</div>`
      + `<div class="tooltip-combo">mean degree ${c.degree.toFixed(2)} ± ${c.degree_sd.toFixed(2)} · `
      + `${c.seeds} seeds · ${Math.round(c.originated).toLocaleString()} bundles/run</div>${rows}`;
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
  const ordered = [...cells].sort((a, b) => a.n - b.n || a.horizon - b.horizon);
  table.innerHTML = `<thead><tr><th>n</th><th>Horizon coeff</th><th>Mean degree</th>`
    + series.map((s) => `<th>${s.short} %</th>`).join("") + `</tr></thead><tbody>`
    + ordered.map((c) => `<tr><td>${c.n}</td><td>${c.horizon}</td><td>${c.degree.toFixed(2)}</td>`
      + series.map((s) => `<td>${c[s.key].toFixed(1)} ± ${c[s.key + "_sd"].toFixed(1)}</td>`).join("")
      + `</tr>`).join("")
    + `</tbody>`;
  document.getElementById("tables").appendChild(table);
})();
</script>
"""


def render(out_path, title, subtitle, footnote, cells):
    rounded = [{k: (round(v, 4) if isinstance(v, float) else v) for k, v in c.items()} for c in cells]
    meta = [{"key": key, "label": label, "short": SHORT[key]} for key, label, _ in SERIES]

    html = TEMPLATE
    for token, value in [
        ("__TITLE__", title),
        ("__SUBTITLE__", subtitle),
        ("__FOOTNOTE__", f'    <p class="footnote">{footnote}</p>\n' if footnote else ""),
        ("__LIGHT_VARS__", "\n".join(f"    --series-{k}: {v};" for k, v in LIGHT.items())),
        ("__DARK_VARS__", "\n".join(f"    --series-{k}: {v};" for k, v in DARK.items())),
        ("__SERIES__", json.dumps(meta)),
        ("__CELLS__", json.dumps(rounded)),
        ("__MARKER__", json.dumps({"at": PERCOLATION_DEGREE, "label": "percolation threshold"})),
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
    ap.add_argument("--subtitle", help="default names the protocol and the seed count read from the CSV")
    ap.add_argument("--footnote",
                    default="A bundle reaches a tower at hand-off, before any ack exists; \"finished\" means it "
                            "left circulation by any route — reached a tower, went by satellite, or was dropped. "
                            "Completion rate and \"reached a tower, % of originated\" differ by the bundles still "
                            "in flight at the cutoff, which is why both are shown; acked and ack lost leave out acks "
                            "still in flight. Degree, truth and belief are averaged over each run. Points are cells "
                            "connected in degree order within a balloon count — not a fitted curve.")
    args = ap.parse_args()

    cells = load_cells(args.csv_path)
    seeds = min(c["seeds"] for c in cells)
    subtitle = args.subtitle or (
        f"dv-dtn (shipped defaults), real wind, 24 sim-h runs — mean of {seeds} seeds per point, "
        f"band ±1 sd — protocol_sweep.rs"
    )
    render(args.out, args.title, subtitle, args.footnote, cells)


if __name__ == "__main__":
    main()
