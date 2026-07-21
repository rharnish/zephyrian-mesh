#!/usr/bin/env python3
"""
Renders a self-contained, interactive HTML line chart (hover tooltips,
crosshair, light/dark theme) from a connectivity-sweep results CSV — the
HTML equivalent of plot_sweep.py's static PNG, for the sweep-chart*.html
files under experiments/results-rust/ and experiments/results/. Balloon
count is always placed on a true numeric x-axis (proportional to value,
not evenly-spaced by index) — this reads better than index-spacing for
sweeps whose balloon counts aren't evenly spaced (e.g. a dense sub-sweep
mixed with a sparse full grid).

One line per horizon coefficient by default. Pass --split-by to add a
second swept column (e.g. fallbackTimeoutMin) as additional series per
coefficient, distinguished by line style (solid/dashed/dotted) instead of
color, so e.g. a 2-coefficient x 2-timeout grid renders as 4 clearly
related series rather than 4 arbitrary ones.

Balloon counts not swept for a given series (e.g. a denser sub-sweep added
only for some coefficients) are left as gaps rather than errors, matching
plot_sweep.py.

Usage:
    python3 experiments/plot_sweep_html.py experiments/connectivity-sweep-results-rust.csv \
        --out experiments/results-rust/sweep-chart-transition.html \
        --config experiments/sweep-config.json --split-by fallbackTimeoutMin \
        --title "Radio delivery % by balloon count"

    python3 experiments/plot_sweep_html.py experiments/connectivity-sweep-results-rust.csv \
        --out experiments/results-rust/sweep-chart.html \
        --coeffs 3.4,3.6,3.8,4.0 --n-values 50,100,200,400,800,1600
"""

import argparse
import csv
import json
import os
import sys
from statistics import mean

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from plot_sweep import load_rows  # noqa: E402

# Canonical coefficient -> color slot, so a coefficient keeps the same color
# across charts regardless of which other coefficients are/aren't present.
CANONICAL_COEFFS = [3.4, 3.6, 3.8, 4.0]
LIGHT_COLORS = ["#2a78d6", "#008300", "#e87ba4", "#eda100"]
DARK_COLORS = ["#3987e5", "#008300", "#d55181", "#c98500"]
FALLBACK_LIGHT = ["#6b4fbb", "#1a9e8f", "#c2542a", "#8a8a20"]
FALLBACK_DARK = ["#8a72d8", "#2fc0ae", "#e07248", "#adad33"]

# solid, dashed, dotted, dash-dot — cycled if a split column has >4 values.
DASH_PATTERNS = [None, "5 4", "1.5 3", "6 2 1.5 2"]


def color_for_coeff(coeff, seen_order):
    if coeff in CANONICAL_COEFFS:
        i = CANONICAL_COEFFS.index(coeff)
        return LIGHT_COLORS[i], DARK_COLORS[i]
    i = seen_order.index(coeff) % len(FALLBACK_LIGHT)
    return FALLBACK_LIGHT[i], FALLBACK_DARK[i]


def build_series(rows, coeffs, n_values, split_col, split_values):
    """Returns {(coeff, split_value): [pctRadio or None, ...]} aligned to n_values.
    split_value is None (and split_col unused) when not splitting."""
    series = {}
    split_iter = split_values if split_col else [None]
    for c in coeffs:
        for sv in split_iter:
            vals = []
            for n in n_values:
                matches = [
                    r["pctRadio"]
                    for r in rows
                    if r["horizonCoeff"] == c
                    and r["nBalloons"] == n
                    and (split_col is None or str(r[split_col]) == str(sv))
                ]
                vals.append(mean(matches) if matches else None)
            series[(c, sv)] = vals
    return series


TEMPLATE = """<title>__TITLE__</title>
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
__DARK_VARS__
  }

  * { box-sizing: border-box; }

  .card {
    max-width: 880px;
    margin: 0 auto;
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 28px 28px 20px;
  }
  h1 { font-size: 17px; font-weight: 600; margin: 0 0 2px; }
  .sub { font-size: 13px; color: var(--text-secondary); margin: 0 0 20px; }

  .legend { display: flex; gap: 18px; flex-wrap: wrap; margin-bottom: 6px; }
  .legend-item { display: flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--text-secondary); }
  .swatch { width: 18px; height: 0; border-top-width: 2.5px; border-top-style: solid; flex: none; }
  .swatch.dashed { border-top-style: dashed; }
  .swatch.dotted { border-top-style: dotted; }

  .chart-wrap { position: relative; }
  svg { display: block; width: 100%; height: auto; overflow: visible; }
  .gridline { stroke: var(--grid); stroke-width: 1; }
  .baseline { stroke: var(--baseline); stroke-width: 1; }
  .axis-label { fill: var(--text-muted); font-size: 11px; }
  .series-line { fill: none; stroke-width: 2.25; }
  .series-dot { stroke: var(--surface-1); stroke-width: 1.5; }
  .end-label { font-size: 11px; font-weight: 600; }

  .crosshair { stroke: var(--text-muted); stroke-width: 1; stroke-dasharray: 2 3; opacity: 0; pointer-events: none; }
  .hover-dot { opacity: 0; pointer-events: none; }
  .hover-target { fill: transparent; }

  .tooltip {
    position: absolute;
    pointer-events: none;
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 8px 10px;
    font-size: 12px;
    box-shadow: 0 4px 16px rgba(0,0,0,0.12);
    opacity: 0;
    transition: opacity 0.08s ease;
    min-width: 165px;
  }
  .tooltip-title { font-weight: 600; margin-bottom: 4px; color: var(--text-primary); }
  .tooltip-row { display: flex; justify-content: space-between; gap: 14px; padding: 1px 0; color: var(--text-secondary); }
  .tooltip-row .val { color: var(--text-primary); font-variant-numeric: tabular-nums; font-weight: 600; }

  .footnote { font-size: 11.5px; color: var(--text-muted); margin-top: 14px; line-height: 1.5; }
</style>

<div class="viz-root" id="root-chart">
  <div class="card">
    <h1>__TITLE__</h1>
    <p class="sub">__SUBTITLE__</p>
    <div class="legend" id="legend"></div>
    <div class="chart-wrap">
      <svg id="chart" viewBox="0 0 800 380" preserveAspectRatio="xMidYMid meet"></svg>
      <div class="tooltip" id="tooltip"></div>
    </div>
__FOOTNOTE__
  </div>
</div>

<script>
(function () {
  const xLabels = __X_LABELS__;
  const xNums = xLabels.map(Number);
  const data = __DATA__;
  const seriesOrder = __SERIES_ORDER__;
  const seriesLabel = __SERIES_LABEL__;
  const colorVar = __COLOR_VAR__;
  const dashPattern = __DASH_PATTERN__;

  const root = document.getElementById("root-chart");
  const cs = getComputedStyle(root);
  const color = (k) => cs.getPropertyValue(colorVar[k]).trim();

  const legend = document.getElementById("legend");
  seriesOrder.forEach((k) => {
    const dp = dashPattern[k];
    const cls = dp === "1.5 3" ? "dotted" : dp ? "dashed" : "";
    const item = document.createElement("div");
    item.className = "legend-item";
    item.innerHTML = `<span class="swatch ${cls}" style="border-top-color:${color(k)}"></span>${seriesLabel[k]}`;
    legend.appendChild(item);
  });

  const W = 800, H = 380;
  const padL = 40, padR = 46, padT = 16, padB = 34;
  const plotW = W - padL - padR, plotH = H - padT - padB;
  const xN = xLabels.length;
  const xMin = xNums[0], xMax = xNums[xNums.length - 1];
  const xPos = (i) => padL + plotW * (xNums[i] - xMin) / (xMax - xMin);
  const yMax = 100;
  const yPos = (v) => padT + plotH * (1 - v / yMax);

  const svg = document.getElementById("chart");
  const ns = "http://www.w3.org/2000/svg";
  const el = (tag, attrs) => {
    const e = document.createElementNS(ns, tag);
    for (const k in attrs) e.setAttribute(k, attrs[k]);
    return e;
  };

  for (let v = 0; v <= 100; v += 25) {
    const y = yPos(v);
    svg.appendChild(el("line", { x1: padL, x2: W - padR, y1: y, y2: y, class: v === 0 ? "baseline" : "gridline" }));
    const label = el("text", { x: padL - 8, y: y + 4, class: "axis-label", "text-anchor": "end" });
    label.textContent = v + "%";
    svg.appendChild(label);
  }
  xLabels.forEach((lab, i) => {
    const label = el("text", { x: xPos(i), y: H - padB + 20, class: "axis-label", "text-anchor": "middle" });
    label.textContent = lab;
    svg.appendChild(label);
  });
  const xTitle = el("text", { x: padL + plotW / 2, y: H - 2, class: "axis-label", "text-anchor": "middle" });
  xTitle.textContent = "Balloon count";
  svg.appendChild(xTitle);

  seriesOrder.forEach((k) => {
    const vals = data[k];
    // break the line across gaps instead of interpolating over missing points
    let segIdx = [];
    const flushSeg = () => {
      if (segIdx.length === 0) return;
      const pts = segIdx.map((i) => `${xPos(i)},${yPos(vals[i])}`).join(" ");
      const attrs = { points: pts, class: "series-line", stroke: color(k) };
      if (dashPattern[k]) attrs["stroke-dasharray"] = dashPattern[k];
      svg.appendChild(el("polyline", attrs));
      segIdx.forEach((i) => svg.appendChild(el("circle", { cx: xPos(i), cy: yPos(vals[i]), r: 3.5, class: "series-dot", fill: color(k) })));
      segIdx = [];
    };
    vals.forEach((v, i) => {
      if (v === null) { flushSeg(); return; }
      segIdx.push(i);
    });
    flushSeg();
    let lastI = vals.length - 1;
    while (lastI >= 0 && vals[lastI] === null) lastI--;
    if (lastI >= 0) {
      const t = el("text", { x: xPos(lastI) + 8, y: yPos(vals[lastI]) + 4, class: "end-label", fill: color(k) });
      t.textContent = vals[lastI].toFixed(0) + "%";
      svg.appendChild(t);
    }
  });

  const crosshair = el("line", { x1: 0, x2: 0, y1: padT, y2: H - padB, class: "crosshair" });
  svg.appendChild(crosshair);
  const hoverDots = seriesOrder.map((k) => {
    const c = el("circle", { r: 5.5, class: "hover-dot", fill: color(k), stroke: cs.getPropertyValue("--surface-1").trim(), "stroke-width": 2 });
    svg.appendChild(c);
    return c;
  });

  const tooltip = document.getElementById("tooltip");
  const chartWrap = svg.closest(".chart-wrap");

  const positions = xLabels.map((_, i) => xPos(i));
  const bounds = [padL];
  for (let i = 0; i < positions.length - 1; i++) bounds.push((positions[i] + positions[i + 1]) / 2);
  bounds.push(padL + plotW);

  xLabels.forEach((lab, i) => {
    const zx = bounds[i], zw = bounds[i + 1] - bounds[i];
    const target = el("rect", { x: zx, y: padT, width: zw, height: plotH, class: "hover-target" });
    target.addEventListener("mouseenter", () => showTooltip(i));
    target.addEventListener("mousemove", (ev) => positionTooltip(ev, i));
    target.addEventListener("mouseleave", hideTooltip);
    svg.appendChild(target);
  });

  function showTooltip(i) {
    crosshair.setAttribute("x1", xPos(i));
    crosshair.setAttribute("x2", xPos(i));
    crosshair.style.opacity = 1;
    hoverDots.forEach((d, idx) => {
      const k = seriesOrder[idx];
      const v = data[k][i];
      if (v === null) { d.style.opacity = 0; return; }
      d.setAttribute("cx", xPos(i));
      d.setAttribute("cy", yPos(v));
      d.style.opacity = 1;
    });
    const rows = seriesOrder
      .map((k) => {
        const v = data[k][i];
        return `<div class="tooltip-row"><span>${seriesLabel[k]}</span><span class="val">${v === null ? "—" : v.toFixed(1) + "%"}</span></div>`;
      })
      .join("");
    tooltip.innerHTML = `<div class="tooltip-title">${xLabels[i]} balloons</div>${rows}`;
    tooltip.style.opacity = 1;
  }
  function positionTooltip(ev, i) {
    const rect = chartWrap.getBoundingClientRect();
    const x = ev.clientX - rect.left;
    const y = ev.clientY - rect.top;
    const flip = x > rect.width * 0.62;
    tooltip.style.left = (flip ? x - tooltip.offsetWidth - 14 : x + 14) + "px";
    tooltip.style.top = Math.max(0, y - 40) + "px";
  }
  function hideTooltip() {
    crosshair.style.opacity = 0;
    hoverDots.forEach((d) => (d.style.opacity = 0));
    tooltip.style.opacity = 0;
  }
})();
</script>
"""


def render(out_path, title, subtitle, footnote, x_labels, series, split_col):
    seen_coeffs = []
    for (c, _sv) in series.keys():
        if c not in seen_coeffs:
            seen_coeffs.append(c)

    series_order = []
    series_label = {}
    color_var = {}
    dash_pattern = {}
    data = {}
    light_vars, dark_vars = [], []
    var_index = {}

    split_values_seen = []
    for (_c, sv) in series.keys():
        if sv is not None and sv not in split_values_seen:
            split_values_seen.append(sv)

    for coeff in seen_coeffs:
        light, dark = color_for_coeff(coeff, seen_coeffs)
        var_name = f"--series-c{str(coeff).replace('.', 'p')}"
        var_index[coeff] = var_name
        light_vars.append(f"    {var_name}: {light};")
        dark_vars.append(f"    {var_name}: {dark};")

    for (coeff, sv) in series.keys():
        key = f"{coeff}|{sv}" if split_col else f"{coeff}"
        series_order.append(key)
        label = f"coeff {coeff}" if not split_col else f"coeff {coeff}, {split_col} {sv}"
        series_label[key] = label
        color_var[key] = var_index[coeff]
        if split_col:
            dash_pattern[key] = DASH_PATTERNS[split_values_seen.index(sv) % len(DASH_PATTERNS)]
        else:
            dash_pattern[key] = None
        data[key] = [None if v is None else round(v, 5) for v in series[(coeff, sv)]]

    html = TEMPLATE
    html = html.replace("__TITLE__", title)
    html = html.replace("__SUBTITLE__", subtitle)
    html = html.replace("__FOOTNOTE__", f'    <p class="footnote">{footnote}</p>\n' if footnote else "")
    html = html.replace("__LIGHT_VARS__", "\n".join(light_vars))
    html = html.replace("__DARK_VARS__", "\n".join(dark_vars))
    html = html.replace("__X_LABELS__", json.dumps(x_labels))
    html = html.replace("__DATA__", json.dumps(data))
    html = html.replace("__SERIES_ORDER__", json.dumps(series_order))
    html = html.replace("__SERIES_LABEL__", json.dumps(series_label))
    html = html.replace("__COLOR_VAR__", json.dumps(color_var))
    html = html.replace("__DASH_PATTERN__", json.dumps(dash_pattern))

    with open(out_path, "w") as f:
        f.write(html)
    print(f"Wrote {out_path}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv_path")
    ap.add_argument("--out", required=True, help="output HTML path")
    ap.add_argument("--config", help="sweep-config.json to read default --coeffs/--n-values/--split-values from")
    ap.add_argument("--coeffs", help="comma-separated horizon coefficients to plot (default: all present in CSV, or from --config)")
    ap.add_argument("--n-values", help="comma-separated balloon counts to plot (default: all present in CSV, or from --config)")
    ap.add_argument("--split-by", help="CSV column to split each coefficient into multiple line-style-differentiated series (e.g. fallbackTimeoutMin)")
    ap.add_argument("--split-values", help="comma-separated values of --split-by to include (default: from --config if --split-by matches its grid key, else all present)")
    ap.add_argument("--title", default="Radio delivery % by balloon count")
    ap.add_argument("--subtitle", default="")
    ap.add_argument("--footnote", default="")
    args = ap.parse_args()

    rows = load_rows(args.csv_path)

    config = json.load(open(args.config)) if args.config else {}
    coeffs = (
        [float(c) for c in args.coeffs.split(",")]
        if args.coeffs
        else [float(c) for c in config["horizonCoeffs"]]
        if "horizonCoeffs" in config
        else sorted({r["horizonCoeff"] for r in rows})
    )
    n_values = (
        [int(n) for n in args.n_values.split(",")]
        if args.n_values
        else [int(n) for n in config["nBalloons"]]
        if "nBalloons" in config
        else sorted({r["nBalloons"] for r in rows})
    )

    split_col = args.split_by
    split_values = None
    if split_col:
        config_key = split_col[0].lower() + split_col[1:]  # tolerate exact CSV column name
        if args.split_values:
            split_values = [int(v) if v.lstrip("-").isdigit() else v for v in args.split_values.split(",")]
        elif config_key in config:
            split_values = config[config_key]
        else:
            split_values = sorted({r[split_col] for r in rows})

    series = build_series(rows, coeffs, n_values, split_col, split_values)
    x_labels = [str(n) for n in n_values]
    render(args.out, args.title, args.subtitle, args.footnote, x_labels, series, split_col)


if __name__ == "__main__":
    main()
