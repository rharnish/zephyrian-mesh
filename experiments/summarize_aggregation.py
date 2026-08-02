#!/usr/bin/env python3
"""
Aggregates an aggregation_sweep.rs results CSV into Markdown tables, ready to
paste into aggregation-summary.md.

The headline analysis here is **paired**, and that is not a stylistic choice.
Variance in this simulation is dominated by how many balloons happen to sit in
tower range, which is a property of the balloon field and correlates with
delivered/round at r = 0.94. Comparing marginal means across configurations
would bury a real effect under that. But the protocol draws from an RNG stream
independent of the world's (see MeshProtocol::reseed), so every configuration
at a given seed runs over an *identical* field — which means differences can be
taken per seed, cancelling the dominant variance entirely before averaging.

So: unpaired tables to describe levels, paired tables to claim effects.

This script only computes; it writes no prose. Interpretation, comparison to
prior runs and limitations still need a human/LLM pass over the output — same
division of labour as summarize_sweep.py.

Usage:
    python3 experiments/summarize_aggregation.py experiments/aggregation-sweep-results.csv
    python3 experiments/summarize_aggregation.py <csv> --tower 4
"""

import argparse
import csv
import sys
from collections import defaultdict
from statistics import mean, stdev

MESH_VALUES = [1, 2, 4, 8]
ACKS = ["source-routed", "digest"]


def load(path):
    with open(path, newline="") as f:
        rows = list(csv.DictReader(f))
    for r in rows:
        for k in ("seed", "nBalloons", "towerContact", "meshHop", "rounds",
                  "delivered", "blocked", "ackLost", "originated", "satellite"):
            r[k] = int(r[k])
        for k in ("completionRate", "meanDegree", "groundedPct",
                  "meanTowerAdjacent", "deliveryCeilingPerRound"):
            r[k] = float(r[k])
    return rows


def fmt_pm(values, pct=True, places=1):
    """mean ± sd over seeds. sd is omitted for a single sample, where it would
    be a false precision rather than a missing one."""
    if not values:
        return "—"
    m = mean(values)
    scale = 100.0 if pct else 1.0
    if len(values) < 2:
        return f"{m * scale:.{places}f}"
    return f"{m * scale:.{places}f} ± {stdev(values) * scale:.{places}f}"


def key(r):
    return (r["nBalloons"], r["ackPolicy"], r["towerContact"], r["meshHop"])


def levels_table(rows, n, tower, out):
    """Completion by mesh batch size and ack policy, at one density."""
    by = defaultdict(list)
    for r in rows:
        if r["nBalloons"] == n and r["towerContact"] == tower:
            by[(r["ackPolicy"], r["meshHop"])].append(r["completionRate"])

    if not by:
        return
    out.append(f"\n**n = {n}, tower_contact = {tower}** — completion %, mean ± sd across seeds\n")
    out.append("| ack | " + " | ".join(f"mesh={m}" for m in MESH_VALUES) + " |")
    out.append("|---|" + "---|" * len(MESH_VALUES))
    for ack in ACKS:
        cells = [fmt_pm(by.get((ack, m), [])) for m in MESH_VALUES]
        out.append(f"| {ack} | " + " | ".join(cells) + " |")


def paired_table(rows, out, tower=4):
    """Per-seed differences, which cancel the between-field variance."""
    # index by (seed, n, ack, tower, mesh) so pairs can be looked up exactly
    idx = {(r["seed"],) + key(r): r for r in rows}
    seeds = sorted({r["seed"] for r in rows})
    ns = sorted({r["nBalloons"] for r in rows})

    out.append(f"\n### Effect of the ack digest, paired by seed (tower_contact = {tower})\n")
    out.append("Each cell is the mean over seeds of (digest − source-routed) at that")
    out.append("seed, in completion percentage points. Positive favours the digest.\n")
    out.append("| n | " + " | ".join(f"mesh={m}" for m in MESH_VALUES) + " |")
    out.append("|---|" + "---|" * len(MESH_VALUES))
    for n in ns:
        cells = []
        for m in MESH_VALUES:
            deltas = []
            for s in seeds:
                a = idx.get((s, n, "digest", tower, m))
                b = idx.get((s, n, "source-routed", tower, m))
                if a and b:
                    deltas.append(a["completionRate"] - b["completionRate"])
            cells.append(fmt_pm(deltas))
        out.append(f"| {n} | " + " | ".join(cells) + " |")

    out.append(f"\n### Effect of mesh batching, paired by seed (tower_contact = {tower})\n")
    out.append("Mean over seeds of (mesh=N − mesh=1) at that seed, completion points.\n")
    out.append("| n | ack | " + " | ".join(f"mesh={m}" for m in MESH_VALUES[1:]) + " |")
    out.append("|---|---|" + "---|" * (len(MESH_VALUES) - 1))
    for n in ns:
        for ack in ACKS:
            cells = []
            for m in MESH_VALUES[1:]:
                deltas = []
                for s in seeds:
                    a = idx.get((s, n, ack, tower, m))
                    b = idx.get((s, n, ack, tower, 1))
                    if a and b:
                        deltas.append(a["completionRate"] - b["completionRate"])
                cells.append(fmt_pm(deltas))
            out.append(f"| {n} | {ack} | " + " | ".join(cells) + " |")


def ceiling_table(rows, out, tower=4):
    """How close delivery runs to what the last hop alone could pass — the
    difference between 'the mesh is the constraint' and 'the ground link is'."""
    by = defaultdict(list)
    for r in rows:
        if r["towerContact"] != tower:
            continue
        served = r["delivered"] / r["rounds"]
        ceiling = r["deliveryCeilingPerRound"]
        if ceiling > 0:
            by[(r["nBalloons"], r["ackPolicy"], r["meshHop"])].append(served / ceiling)

    if not by:
        return
    out.append(f"\n### Share of the last-hop ceiling actually used (tower_contact = {tower})\n")
    out.append("Delivery as a fraction of what the tower-adjacent population could pass.")
    out.append("Well under 100% means the mesh is the binding constraint, not the ground link.\n")
    ns = sorted({n for (n, _, _) in by})
    out.append("| n | ack | " + " | ".join(f"mesh={m}" for m in MESH_VALUES) + " |")
    out.append("|---|---|" + "---|" * len(MESH_VALUES))
    for n in ns:
        for ack in ACKS:
            cells = [fmt_pm(by.get((n, ack, m), [])) for m in MESH_VALUES]
            out.append(f"| {n} | {ack} | " + " | ".join(cells) + " |")


def ack_loss_table(rows, out, tower=4):
    by = defaultdict(list)
    for r in rows:
        if r["towerContact"] == tower and r["delivered"] > 0:
            by[(r["nBalloons"], r["ackPolicy"], r["meshHop"])].append(
                r["ackLost"] / r["delivered"]
            )
    if not by:
        return
    out.append(f"\n### Delivered but unacknowledged (tower_contact = {tower})\n")
    out.append("Share of delivered bundles whose receipt never got home — the band")
    out.append("where a balloon cannot tell 'never arrived' from 'arrived, receipt died'.\n")
    ns = sorted({n for (n, _, _) in by})
    out.append("| n | ack | " + " | ".join(f"mesh={m}" for m in MESH_VALUES) + " |")
    out.append("|---|---|" + "---|" * len(MESH_VALUES))
    for n in ns:
        for ack in ACKS:
            cells = [fmt_pm(by.get((n, ack, m), [])) for m in MESH_VALUES]
            out.append(f"| {n} | {ack} | " + " | ".join(cells) + " |")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("csv")
    ap.add_argument("--tower", type=int, default=4,
                    help="tower_contact to focus the paired tables on (default 4)")
    args = ap.parse_args()

    rows = load(args.csv)
    if not rows:
        sys.exit("no rows in CSV")

    seeds = sorted({r["seed"] for r in rows})
    ns = sorted({r["nBalloons"] for r in rows})
    towers = sorted({r["towerContact"] for r in rows})
    rounds = sorted({r["rounds"] for r in rows})

    out = []
    out.append(f"{len(rows)} rows · {len(seeds)} seeds · n ∈ {ns} · "
               f"tower_contact ∈ {towers} · rounds ∈ {rounds}")

    # Flag partial grids loudly: a resumable sweep read mid-run will silently
    # produce lopsided averages otherwise.
    expected = len(seeds) * len(ns) * len(towers) * len(MESH_VALUES) * len(ACKS)
    if len(rows) != expected:
        out.append(f"\n> **Partial grid**: {len(rows)} of {expected} expected rows. "
                   f"Cells below average over whatever landed, so treat them as provisional.")

    out.append("\n## Levels\n")
    for n in ns:
        for t in towers:
            levels_table(rows, n, t, out)

    out.append("\n## Effects (paired by seed)")
    paired_table(rows, out, tower=args.tower)
    ceiling_table(rows, out, tower=args.tower)
    ack_loss_table(rows, out, tower=args.tower)

    print("\n".join(out))


if __name__ == "__main__":
    main()
