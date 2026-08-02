#!/usr/bin/env python3
"""Tables for the wind sweep: does real weather change the protocol ordering?

    python3 experiments/summarize_wind.py experiments/wind-sweep-results.csv

The design point this script leans on: every (wind, seed) cell runs every
protocol over an identical balloon field, because the protocol's RNG is
independent of the world's. So protocol differences are taken *within* a cell
before averaging, which cancels the field-to-field variance that otherwise
dominates everything in this simulation.

Wind conditions are NOT paired with each other — balloons advect differently
once wind is applied, so "zero wind seed 3" and "06:00 seed 3" are different
fields. Comparisons across wind are therefore unpaired and carry the full
spread; that is stated rather than hidden.
"""
import csv, sys, math
from collections import defaultdict

PROTOCOL_ORDER = [
    "dv-dtn",
    "dv-dtn-digest-batch",
    "dv-dtn-reactive",
    "dv-dtn-link-state",
    "spray-4",
    "spray-16",
]


def mean(v):
    return sum(v) / len(v) if v else float("nan")


def sd(v):
    if len(v) < 2:
        return 0.0
    m = mean(v)
    return math.sqrt(sum((x - m) ** 2 for x in v) / (len(v) - 1))


def sem(v):
    return sd(v) / math.sqrt(len(v)) if len(v) > 1 else 0.0


def load(path):
    rows = []
    with open(path) as f:
        for r in csv.DictReader(f):
            for k in ("completionRate", "meanDegree", "beliefHopsMean", "stallStaleNextHop",
                      "droppedTtl", "delivered", "satellite"):
                try:
                    r[k] = float(r[k])
                except (ValueError, KeyError):
                    r[k] = float("nan")
            r["seed"] = int(r["seed"])
            rows.append(r)
    return rows


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "experiments/wind-sweep-results.csv"
    rows = load(path)

    winds = sorted({r["wind"] for r in rows}, key=lambda w: (w != "none", w))
    protos = [p for p in PROTOCOL_ORDER if any(r["protocol"] == p for r in rows)]
    seeds = sorted({r["seed"] for r in rows})
    by = {(r["wind"], r["protocol"], r["seed"]): r for r in rows}

    print(f"{len(rows)} rows: {len(winds)} winds x {len(seeds)} seeds x {len(protos)} protocols")
    print(f"n={rows[0]['nBalloons']}, {rows[0]['rounds']} rounds\n")

    # ---- completion by protocol x wind -----------------------------------
    print("## Completion %, mean +/- sd across seeds\n")
    w = max(len(p) for p in protos) + 2
    print(f"{'protocol':<{w}}" + "".join(f"{lbl[-8:] if lbl != 'none' else 'zero':>16}" for lbl in winds))
    print("-" * (w + 16 * len(winds)))
    for p in protos:
        line = f"{p:<{w}}"
        for wd in winds:
            v = [by[(wd, p, s)]["completionRate"] * 100 for s in seeds if (wd, p, s) in by]
            line += f"{mean(v):>10.1f}±{sd(v):<5.1f}"
        print(line)

    # ---- the headline question -------------------------------------------
    print("\n## Zero wind vs. real weather (all real fields pooled)\n")
    print(f"{'protocol':<{w}}{'zero wind':>16}{'real wind':>18}{'difference':>16}")
    print("-" * (w + 50))
    for p in protos:
        z = [by[("none", p, s)]["completionRate"] * 100 for s in seeds if ("none", p, s) in by]
        r = [by[(wd, p, s)]["completionRate"] * 100
             for wd in winds if wd != "none" for s in seeds if (wd, p, s) in by]
        d = mean(r) - mean(z)
        # Unpaired: se of a difference of two independent means.
        se = math.sqrt(sem(z) ** 2 + sem(r) ** 2)
        flag = "" if abs(d) < 2 * se else "  <-- outside 2 s.e."
        print(f"{p:<{w}}{mean(z):>10.1f}±{sd(z):<5.1f}{mean(r):>12.1f}±{sd(r):<5.1f}"
              f"{d:>+11.1f}±{se:<4.1f}{flag}")

    # ---- ordering: does it ever inverEt? ----------------------------------
    print("\n## Does routing ever lose to replication?\n")
    best_spray = "spray-16" if "spray-16" in protos else protos[-1]
    inversions = 0
    cells = 0
    worst = None
    for wd in winds:
        for s in seeds:
            a, b = (wd, "dv-dtn", s), (wd, best_spray, s)
            if a not in by or b not in by:
                continue
            cells += 1
            gap = (by[a]["completionRate"] - by[b]["completionRate"]) * 100
            if gap < 0:
                inversions += 1
            if worst is None or gap < worst[0]:
                worst = (gap, wd, s)
    print(f"dv-dtn vs {best_spray}, compared within each (wind, seed) cell:")
    print(f"  {cells - inversions}/{cells} cells favour dv-dtn; {inversions} inversions")
    if worst:
        print(f"  narrowest margin: {worst[0]:+.1f} points at wind={worst[1]} seed={worst[2]}")

    # ---- where did the churn actually go? --------------------------------
    print("\n## Churn reaching the protocol (dv-dtn)\n")
    print(f"{'wind':<22}{'degree':>9}{'stale next hop':>17}{'belief hops':>13}{'droppedTtl':>12}")
    print("-" * 73)
    for wd in winds:
        v = [by[(wd, "dv-dtn", s)] for s in seeds if (wd, "dv-dtn", s) in by]
        if not v:
            continue
        print(f"{wd:<22}{mean([x['meanDegree'] for x in v]):>9.2f}"
              f"{mean([x['stallStaleNextHop'] for x in v]):>17.0f}"
              f"{mean([x['beliefHopsMean'] for x in v]):>13.2f}"
              f"{mean([x['droppedTtl'] for x in v]):>12.1f}")

    # ---- how much does weather matter vs which field you drew? -----------
    print("\n## Variance: between weather fields vs between seeds\n")
    print(f"{'protocol':<{w}}{'sd across winds':>18}{'sd across seeds':>18}")
    print("-" * (w + 36))
    for p in protos:
        # sd of the per-wind means (real fields only) = between-weather spread
        wind_means = [mean([by[(wd, p, s)]["completionRate"] * 100
                            for s in seeds if (wd, p, s) in by])
                      for wd in winds if wd != "none"]
        # mean of within-wind sds = typical between-seed spread
        seed_sds = [sd([by[(wd, p, s)]["completionRate"] * 100
                        for s in seeds if (wd, p, s) in by])
                    for wd in winds if wd != "none"]
        print(f"{p:<{w}}{sd(wind_means):>18.2f}{mean(seed_sds):>18.2f}")
    print("\nIf the first column is much smaller than the second, which weather you")
    print("get matters far less than which balloon field you drew — i.e. the wind")
    print("result generalises rather than describing one afternoon.")


if __name__ == "__main__":
    main()
