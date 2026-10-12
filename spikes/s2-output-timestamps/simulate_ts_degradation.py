#!/usr/bin/env python3
"""What if the backend only gave coarse/jittery timestamps? Degrade the measured
(now, ticks) pairs and re-run the DLL. Models: ASIO systemTime from timeGetTime
(1 ms quantization), extra Gaussian wake-up jitter (Windows/macOS callback entry)."""
import sys, json
import numpy as np
from analyze import load, dll, fit_ppm, windowed_ppm

path = sys.argv[1]
d = load(path)
rng = np.random.default_rng(1)
tsec = (d['t_cb'] - d['t_cb'][0]) * 1e-9
base = d['now'].copy()
cases = {
    'pw_now (reference)': base,
    't_cb (callback entry)': d['t_cb'],
    'now quantized to 1 ms': (base // 1_000_000) * 1_000_000,
    'now + N(0,100us)': base + rng.normal(0, 100e3, len(base)).astype(np.int64),
    'now + N(0,500us)': base + rng.normal(0, 500e3, len(base)).astype(np.int64),
}
out = {}
for name, t in cases.items():
    row = {}
    for bw in (0.01, 0.03, 0.1):
        ppm, _ = dll(t, d['ticks'], bw)
        mm = tsec >= max(30, 1.2 / bw)
        x = ppm[mm]
        row[f'dll{bw}'] = {'mean': round(float(x.mean()), 2), 'std': round(float(x.std()), 2), 'pp': round(float(np.ptp(x)), 2)}
    for win in (10.0, 60.0):
        e = windowed_ppm(t[tsec >= 30], d['ticks'][tsec >= 30], win, step_s=win)
        row[f'win{int(win)}'] = {'std': round(float(e.std()), 2), 'pp': round(float(np.ptp(e)), 2)}
    out[name] = row
print(json.dumps(out, indent=1))
