#!/usr/bin/env python3
"""Compare the DAC rate seen by the kernel (ALSA hw_ptr vs its CLOCK_MONOTONIC
tstamp, polled from /proc at 20 Hz) with PipeWire's (now, ticks), same windows."""
import sys, numpy as np
from analyze import load, fit_ppm
d = load(sys.argv[1])
h = np.genfromtxt(sys.argv[2], delimiter=',', names=True, dtype=None, encoding=None)
ts = np.array([int(round(float(x) * 1e9)) for x in h['tstamp']], dtype=np.int64)
hp = np.array(h['hw_ptr'], dtype=np.int64)
# keep only samples where the tstamp moved (fresh pointer updates)
keep = np.concatenate([[True], np.diff(ts) > 0])
ts, hp = ts[keep], hp[keep]
t0 = max(ts[0], d['now'][0]) + 30_000_000_000
t1 = min(ts[-1], d['now'][-1])
mh = (ts >= t0) & (ts <= t1)
mp = (d['now'] >= t0) & (d['now'] <= t1)
print('span s', (t1 - t0) / 1e9)
ppm_h, r_h = fit_ppm(ts[mh], hp[mh])
ppm_p, _ = fit_ppm(d['now'][mp], d['ticks'][mp])
print('whole: kernel hw_ptr %.3f ppm  pipewire ticks %.3f ppm' % (ppm_h, ppm_p))
print('kernel hw_ptr residual vs line: std %.1f us, p-p %.1f us' % (r_h.std(), np.ptp(r_h)))
for win in (60.0, 120.0):
    rows = []
    s = t0
    while s + win * 1e9 <= t1:
        a = (ts >= s) & (ts < s + win * 1e9)
        b = (d['now'] >= s) & (d['now'] < s + win * 1e9)
        rows.append((fit_ppm(ts[a], hp[a])[0], fit_ppm(d['now'][b], d['ticks'][b])[0]))
        s += win * 1e9
    rows = np.array(rows)
    print(f'win {win:.0f}s kernel:', np.round(rows[:, 0], 2))
    print(f'win {win:.0f}s pw    :', np.round(rows[:, 1], 2))
    print(f'win {win:.0f}s  std kernel %.2f pw %.2f  corr %.2f  std(diff) %.2f' % (
        rows[:, 0].std(), rows[:, 1].std(), np.corrcoef(rows[:, 0], rows[:, 1])[0, 1], (rows[:, 0] - rows[:, 1]).std()))
