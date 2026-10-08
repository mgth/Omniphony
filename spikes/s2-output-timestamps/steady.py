#!/usr/bin/env python3
"""Steady-state DLL comparison with fast-start init (skips PipeWire's own 5 s start-up)."""
import sys, json
import numpy as np
from analyze import load, dll_faststart, fit_ppm
path = sys.argv[1]
tcol = sys.argv[2] if len(sys.argv) > 2 else 'now'
d = load(path)
t = (d['t_cb'] - d['t_cb'][0]) * 1e-9
ref = fit_ppm(d[tcol][t >= 60], d['ticks'][t >= 60])[0]
out = {'time_base': tcol, 'whole_run_regression_ppm': round(ref, 3)}
rd = (d['clk_rate_diff'] - 1) * 1e6
for bw in (0.003, 0.01, 0.02, 0.03, 0.1):
    p = dll_faststart(d[tcol], d['ticks'], bw, skip_s=5.0)
    # lock time: first time after which |p - ref| < 2 ppm for good (60 s horizon)
    ok = np.abs(p - ref) < 2.0
    lock = None
    for k in np.where(ok)[0]:
        mm = (t >= t[k]) & (t < t[k] + 30)
        if ok[mm].all():
            lock = float(t[k]); break
    row = {'lock_within_2ppm_s': round(lock, 1) if lock is not None else None}
    for a in (60, 150):
        m = t >= a
        row[f'after{a}s'] = {'mean': round(float(np.nanmean(p[m])), 3), 'std': round(float(np.nanstd(p[m])), 3), 'pp': round(float(np.nanmax(p[m]) - np.nanmin(p[m])), 3)}
    # short-term modulation: p-p within 10 s windows (what a resampler would see)
    m = t >= 150
    w = []
    s = 150.0
    while s + 10 <= t[-1]:
        mm = (t >= s) & (t < s + 10)
        w.append(np.nanmax(p[mm]) - np.nanmin(p[mm]))
        s += 10
    row['pp_within_10s_median'] = round(float(np.median(w)), 3)
    row['pp_within_10s_max'] = round(float(np.max(w)), 3)
    out[f'dll_bw{bw}'] = row
# rate_diff raw and boxcar-averaged as an estimator
m = t >= 60
out['rate_diff_raw_after60s'] = {'mean': round(float(rd[m].mean()), 3), 'std': round(float(rd[m].std()), 3), 'pp': round(float(np.ptp(rd[m])), 3)}
q = float(np.median(d['clk_duration']))
for win in (1, 10, 60):
    n = max(1, int(win * 48000 / q))
    cs = np.cumsum(np.insert(rd, 0, 0)); mv = (cs[n:] - cs[:-n]) / n
    mv = mv[t[:len(mv)] >= 60]
    out[f'rate_diff_mean{win}s_after60s'] = {'std': round(float(mv.std()), 3), 'pp': round(float(np.ptp(mv)), 3)}
print(json.dumps(out, indent=1))
