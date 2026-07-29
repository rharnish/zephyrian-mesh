#!/usr/bin/env python3
"""
Renders a static PNG chart from a protocol_sweep.rs results CSV: omniscient
ground truth (groundedPct, from union-find) vs. what balloons *believe*
(believedGroundedPct) vs. what the real decentralized protocol actually
delivers — completion rate, raw delivered/originated, and delivered-but-
unacknowledged/originated — all plotted against **mean node degree** rather
than balloon count or horizon coefficient, because docs/design/MESH_COMMS_DESIGN.md §1.1
found degree is the real control variable: sweeps over balloon count and
horizon coefficient collapse onto the same curve when read by degree. That
collapse is what makes one clean chart possible instead of a grid of
per-coefficient panels like plot_sweep.py needs for the toy-model CSV.

Delivered/originated and completion rate (delivered/resolved) are both shown
deliberately, not just one: the gap between them *is* the censoring bias from
bundles still legitimately in flight at the cutoff (see BundleStats::
completion_rate's own doc comment) — the same distinction bundle_delivery.rs's
"deliv/orig" vs "completion" columns make.

Points are the sweep's own (horizonCoeff, nBalloons) combos, connected in
degree order — not a fitted curve — so the actual measured shape (including
noise/censoring near the percolation transition) stays visible.

Usage:
    python3 experiments/plot_protocol_sweep.py experiments/protocol-sweep-results.csv \
        --out experiments/protocol-results/truth-vs-belief-vs-delivery.png
"""

import argparse
import csv

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

# Borrowed from the app's own belief-vs-truth overlay (src/main.js BELIEF_COLORS)
# so this chart's color language matches what the running app already uses for
# the same distinction, rather than inventing a new one.
COLOR_TRUTH = "#5fd08a"  # "ok" green — omniscient ground truth (union-find)
COLOR_BELIEF = "#e0a355"  # "unaware" amber — what balloons currently believe
COLOR_ACHIEVED = "#2a78d6"  # blue — completion rate (delivered/resolved)
COLOR_DELIVERED = "#e87ba4"  # pink — raw delivered/originated
COLOR_UNACKED = "#e05561"  # "stale" red — delivered but the ack never came back


def load_rows(csv_path):
    rows = []
    with open(csv_path, newline="") as f:
        for r in csv.DictReader(f):
            for key in ("meanDegree", "groundedPct", "believedGroundedPct", "completionRate",
                        "originated", "delivered", "ackLostCount"):
                r[key] = float(r[key])
            r["deliveredPctOfOriginated"] = (
                100.0 * r["delivered"] / r["originated"] if r["originated"] else 0.0
            )
            r["unackedPctOfOriginated"] = (
                100.0 * r["ackLostCount"] / r["originated"] if r["originated"] else 0.0
            )
            rows.append(r)
    rows.sort(key=lambda r: r["meanDegree"])
    return rows


def plot(rows, out_path, title, subtitle=None):
    fig, ax = plt.subplots(figsize=(8.5, 4.8), dpi=150)

    xs = [r["meanDegree"] for r in rows]
    series = [
        ("Ground truth: actually reachable (union-find)", COLOR_TRUTH, [r["groundedPct"] for r in rows]),
        ("Belief: balloons that think they have a route", COLOR_BELIEF, [r["believedGroundedPct"] for r in rows]),
        ("Achieved: completion rate (delivered/resolved)", COLOR_ACHIEVED,
         [100.0 * r["completionRate"] for r in rows]),
        ("Bundles delivered, % of originated", COLOR_DELIVERED,
         [r["deliveredPctOfOriginated"] for r in rows]),
        ("Bundles delivered without ack, % of originated", COLOR_UNACKED,
         [r["unackedPctOfOriginated"] for r in rows]),
    ]
    for label, color, ys in series:
        ax.plot(xs, ys, marker="o", color=color, linewidth=2, markersize=6, label=label)

    # Percolation threshold tick, same convention as the Controls panel's mesh-health
    # readout in the running app (mean_degree ~4.5).
    ax.axvline(4.5, color="#999999", linewidth=1, linestyle="--", zorder=0)
    ax.text(4.5, 102, "percolation\nthreshold", fontsize=8, color="#777777", ha="center", va="bottom")

    ax.set_xlabel("Mean node degree")
    ax.set_ylabel("%")
    ax.set_ylim(-3, 103)
    ax.set_yticks(range(0, 101, 25))
    ax.grid(axis="y", alpha=0.25)
    ax.spines["top"].set_visible(False)
    ax.spines["right"].set_visible(False)
    ax.legend(frameon=False, loc="lower right", fontsize=9)
    ax.set_title(title, fontsize=12, fontweight="bold", loc="left", pad=28 if subtitle else 14)
    if subtitle:
        ax.text(0, 1.05, subtitle, transform=ax.transAxes, fontsize=9, color="#666666")

    fig.tight_layout()
    fig.savefig(out_path)
    print(f"Wrote {out_path}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv_path")
    ap.add_argument("--out", required=True, help="output PNG path")
    ap.add_argument("--title", default="Truth vs. belief vs. real delivery, by mesh density")
    ap.add_argument("--subtitle", default="C1+C2 decentralized protocol, real wind — protocol_sweep.rs")
    args = ap.parse_args()

    rows = load_rows(args.csv_path)
    plot(rows, args.out, args.title, args.subtitle)


if __name__ == "__main__":
    main()
