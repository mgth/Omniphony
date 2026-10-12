#!/usr/bin/env python3
"""Condense results/quality.tsv into one row per candidate (markdown)."""
import csv
import sys
from collections import defaultdict

path = sys.argv[1] if len(sys.argv) > 1 else "results/quality.tsv"
rows = list(csv.DictReader(open(path), delimiter="\t"))
by = defaultdict(list)
for r in rows:
    by[r["cand"]].append(r)

steady = {"p100", "m100", "p500", "m500", "p2000", "m2000", "mod"}
const = {"p100", "m100", "p500", "m500", "p2000", "m2000"}

print("| candidate | THD+N 1k | THD+N 10k | THD+N 20k | multitone | THD+N 1k @ ratio 1 | varN 20k | passband 20 Hz-20 kHz (dB) | 22 kHz tone: gain / THD+N | delay vs nominal (frames) | delay wander (const ratio) | measured vs reported |")
print("|---|---|---|---|---|---|---|---|---|---|---|---|")
for cand, rs in by.items():
    def worst(sig, scheds=steady):
        v = [float(r["thdn_db"]) for r in rs if r["sig"] == sig and r["sched"] in scheds]
        return max(v) if v else float("nan")
    multi = [r for r in rs if r["sig"] == "multi" and r["sched"] in steady]
    gmin = min(float(r["gain_min_db"]) for r in multi)
    gmax = max(float(r["gain_max_db"]) for r in multi)
    t22 = [r for r in rs if r["sig"] == "t22k" and r["sched"] == "p500"][0]
    offs = [float(r["pos_offset"]) for r in rs if r["sched"] in const and r["sig"] == "t1k"]
    wand = [float(r["pos_wander"]) for r in rs if r["sched"] in const]
    rep = [r["pos_vs_reported"] for r in rs if r["pos_vs_reported"] != "-"]
    repmax = f"{max(float(x) for x in rep):.1e}" if rep else "n/a (not exposed)"
    r1 = worst("t1k", {"r1"})
    varn = worst("t20k", {"varN"})
    print(f"| {cand} | {worst('t1k'):.1f} | {worst('t10k'):.1f} | {worst('t20k'):.1f} | {worst('multi'):.1f} | {r1:.1f} | {varn:.1f} "
          f"| {gmin:+.5f} / {gmax:+.5f} | {float(t22['gain_min_db']):+.2f} / {float(t22['thdn_db']):.1f} "
          f"| {min(offs):.4f} .. {max(offs):.4f} | {max(wand):.1e} | {repmax} |")
