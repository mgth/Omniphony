#!/usr/bin/env python3
"""Spike S2 analysis: callback jitter, DAC-rate estimators, noise vs averaging.

usage: analyze.py data/q1024.csv [--skip SECONDS]
"""
import sys, math, json
import numpy as np

FS = 48000.0


def load(path):
    with open(path) as f:
        lines = [l for l in f if not l.startswith('#')]
    hdr = lines[0].strip().split(',')
    arr = np.genfromtxt(lines[1:], delimiter=',', dtype=np.float64)
    d = {h: arr[:, i] for i, h in enumerate(hdr)}
    # keep int64 precision for big ns values
    raw = [l.strip().split(',') for l in lines[1:]]
    for k in [k for k in ('t_cb', 't_raw', 't_end', 'now', 'clk_nsec', 'clk_next_nsec', 'ticks', 'clk_position') if k in hdr]:
        i = hdr.index(k)
        d[k] = np.array([int(r[i]) for r in raw], dtype=np.int64)
    return d


def pstats(x):
    x = np.asarray(x, dtype=float)
    return dict(mean=float(np.mean(x)), std=float(np.std(x)), p01=float(np.percentile(x, 1)),
                p50=float(np.percentile(x, 50)), p99=float(np.percentile(x, 99)),
                min=float(np.min(x)), max=float(np.max(x)), pp=float(np.ptp(x)))


def fit_ppm(t_ns, pos):
    """Least-squares pos = a + b*t; returns DAC rate offset in ppm vs nominal FS
    (positive = DAC faster than CLOCK_MONOTONIC) and the time residuals (us)."""
    t = (t_ns - t_ns[0]).astype(float) * 1e-9
    p = (pos - pos[0]).astype(float)
    A = np.vstack([np.ones_like(t), t]).T
    (a, b), *_ = np.linalg.lstsq(A, p, rcond=None)
    ppm = (b / FS - 1.0) * 1e6
    resid_frames = p - (a + b * t)
    return ppm, resid_frames / FS * 1e6


def windowed_ppm(t_ns, pos, win_s, step_s=None):
    step_s = step_s or win_s / 4
    t = (t_ns - t_ns[0]).astype(float) * 1e-9
    out = []
    s = 0.0
    while s + win_s <= t[-1]:
        m = (t >= s) & (t < s + win_s)
        if m.sum() > 10:
            out.append(fit_ppm(t_ns[m], pos[m])[0])
        s += step_s
    return np.array(out)


def dll(t_ns, pos, bw_hz, zeta=math.sqrt(0.5)):
    """2nd-order DLL (Adriaensen form) on (time, frame position) pairs.
    Returns the per-update rate estimate in ppm and the filtered-time error (us)."""
    t = (t_ns - t_ns[0]).astype(float)  # ns
    p = (pos - pos[0]).astype(float)
    nspf = 1e9 / FS  # ns per frame estimate
    t_ref = t[0]
    p_ref = p[0]
    ppm = np.zeros(len(t))
    err = np.zeros(len(t))
    for k in range(1, len(t)):
        dn = p[k] - p_ref
        if dn <= 0:
            ppm[k] = ppm[k - 1]
            continue
        T = dn * nspf * 1e-9
        w = 2 * math.pi * bw_hz * T
        b = 2 * zeta * w
        c = w * w
        pred = t_ref + dn * nspf
        e = t[k] - pred
        t_ref = pred + b * e
        p_ref = p[k]
        nspf += c * e / dn
        ppm[k] = (1e9 / FS / nspf - 1.0) * 1e6
        err[k] = e / 1e3
    return ppm, err


def main():
    path = sys.argv[1]
    skip = float(sys.argv[sys.argv.index('--skip') + 1]) if '--skip' in sys.argv else 30.0
    d = load(path)
    t0 = d['t_cb'][0]
    tsec = (d['t_cb'] - t0) * 1e-9
    m = tsec >= skip
    res = {'file': path, 'n': int(len(tsec)), 'duration_s': float(tsec[-1]), 'skip_s': skip}
    q = int(np.median(d['clk_duration']))
    res['quantum'] = q
    # continuity
    dt = np.diff(d['ticks'])
    res['ticks_steps_not_quantum'] = int(np.sum(dt != q))
    res['xrun_final'] = int(d['clk_xrun'][-1])
    res['flags_nonzero'] = int(np.sum(d['clk_flags'] != 0))
    res['now_eq_clk_nsec'] = bool(np.all(d['now'] == d['clk_nsec']))
    res['ticks_minus_position_const'] = bool(np.ptp(d['clk_position'] - d['ticks']) == 0)

    # --- callback timing vs now (cycle start) ---
    lat_us = (d['t_cb'] - d['now'])[m] * 1e-3
    res['cb_minus_now_us'] = pstats(lat_us)
    res['cb_duration_us'] = pstats((d['t_end'] - d['t_cb'])[m] * 1e-3)
    period_nom_us = q / FS * 1e6
    res['cb_interval_minus_nominal_us'] = pstats(np.diff(d['t_cb'][m]) * 1e-3 - period_nom_us)
    res['now_interval_minus_nominal_us'] = pstats(np.diff(d['now'][m]) * 1e-3 - period_nom_us)
    res['next_nsec_minus_next_now_us'] = pstats((d['clk_next_nsec'][m][:-1] - d['now'][m][1:]) * 1e-3)

    # --- whole-run regressions ---
    ppm_now, r_now = fit_ppm(d['now'][m], d['ticks'][m])
    ppm_cb, r_cb = fit_ppm(d['t_cb'][m], d['ticks'][m])
    dac = d['clk_position'][m] - d['clk_delay'][m].astype(np.int64)
    ppm_dac, r_dac = fit_ppm(d['t_cb'][m], dac)
    res['ppm_regress_now_ticks'] = ppm_now
    res['ppm_regress_tcb_ticks'] = ppm_cb
    res['ppm_regress_tcb_pos_minus_clkdelay'] = ppm_dac
    res['resid_us_now_ticks'] = pstats(r_now)
    res['resid_us_tcb_ticks'] = pstats(r_cb)
    res['resid_us_tcb_dacpos'] = pstats(r_dac)
    rd = (d['clk_rate_diff'][m] - 1.0) * 1e6
    res['rate_diff_ppm'] = pstats(rd)

    # --- noise vs averaging time ---
    for win in (10.0, 60.0):
        w_now = windowed_ppm(d['now'][m], d['ticks'][m], win)
        w_cb = windowed_ppm(d['t_cb'][m], d['ticks'][m], win)
        res[f'win{int(win)}_regress_now'] = pstats(w_now)
        res[f'win{int(win)}_regress_tcb'] = pstats(w_cb)
        # rate_diff boxcar mean over the window
        n = max(1, int(win * FS / q))
        cs = np.cumsum(np.insert(rd, 0, 0))
        mv = (cs[n:] - cs[:-n]) / n
        res[f'win{int(win)}_rate_diff_mean'] = pstats(mv[::max(1, n // 4)])

    # --- noise vs averaging time (non-overlapping windows, std and p-p) ---
    taus = {}
    for win in (1.0, 3.0, 10.0, 30.0, 60.0, 120.0):
        e = windowed_ppm(d['now'][m], d['ticks'][m], win, step_s=win)
        if len(e) >= 3:
            taus[str(win)] = {'n': int(len(e)), 'std': float(np.std(e)), 'pp': float(np.ptp(e)),
                              'adev_like': float(np.sqrt(0.5 * np.mean(np.diff(e) ** 2)))}
    res['tau_now_ticks'] = taus
    if 't_raw' in d:
        ppm_raw, r_raw = fit_ppm(d['t_raw'][m], d['ticks'][m])
        res['ppm_regress_raw_ticks'] = ppm_raw
        res['resid_us_raw_ticks'] = pstats(r_raw)
        # rate of CLOCK_MONOTONIC vs CLOCK_MONOTONIC_RAW (NTP slew), ppm
        tm = (d['t_cb'][m] - d['t_cb'][m][0]).astype(float)
        tr = (d['t_raw'][m] - d['t_raw'][m][0]).astype(float)
        A = np.vstack([np.ones_like(tr), tr]).T
        (a, b), *_ = np.linalg.lstsq(A, tm, rcond=None)
        res['mono_vs_raw_ppm'] = (b - 1) * 1e6
        taus_r = {}
        for win in (1.0, 3.0, 10.0, 30.0, 60.0, 120.0):
            e = windowed_ppm(d['t_raw'][m], d['ticks'][m], win, step_s=win)
            if len(e) >= 3:
                taus_r[str(win)] = {'n': int(len(e)), 'std': float(np.std(e)), 'pp': float(np.ptp(e)),
                                    'adev_like': float(np.sqrt(0.5 * np.mean(np.diff(e) ** 2)))}
        res['tau_raw_ticks'] = taus_r
        # windowed mono-vs-raw rate (60 s)
        mr = []
        t = tsec[m]
        s0 = t[0]
        while s0 + 60 <= t[-1]:
            mm = (t >= s0) & (t < s0 + 60)
            x = tr[mm]; y = tm[mm]
            A = np.vstack([np.ones_like(x), x]).T
            (a, b), *_ = np.linalg.lstsq(A, y, rcond=None)
            mr.append((b - 1) * 1e6)
            s0 += 60
        res['mono_vs_raw_ppm_60s_windows'] = [round(v, 3) for v in mr]
        res['win60_regress_raw_series'] = [round(v, 3) for v in windowed_ppm(d['t_raw'][m], d['ticks'][m], 60.0, step_s=60.0)]
        res['win60_regress_now_series'] = [round(v, 3) for v in windowed_ppm(d['now'][m], d['ticks'][m], 60.0, step_s=60.0)]
        for bw in (0.01, 0.03, 0.1):
            ppm_d, e_d = dll(d['t_raw'], d['ticks'], bw)
            mm = tsec >= max(skip, 1.2 / bw)
            res[f'dll_raw_bw{bw}_ppm'] = pstats(ppm_d[mm])
    # --- own DLLs ---
    for bw in (0.01, 0.03, 0.1):
        for src, tt in (('tcb', d['t_cb']), ('now', d['now'])):
            ppm_d, e_d = dll(tt, d['ticks'], bw)
            mm = tsec >= max(skip, 1.2 / bw)  # ~ 5 time constants
            res[f'dll_{src}_bw{bw}_ppm'] = pstats(ppm_d[mm])
            res[f'dll_{src}_bw{bw}_err_us'] = pstats(e_d[mm])
            if src == 'tcb':
                np.save(path.replace('.csv', f'.dll_tcb_bw{bw}.npy'), ppm_d)
    # --- delay ---
    res['pw_time_delay'] = pstats(d['delay'])
    res['pw_time_delay_values'] = sorted(set(int(x) for x in d['delay']))
    res['queued_before'] = sorted(set(int(x) for x in d['queued']))[:5]
    res['queued_after'] = sorted(set(int(x) for x in d['queued_after']))[:5]
    res['buffered'] = sorted(set(int(x) for x in d['buffered']))[:5]
    res['size'] = sorted(set(int(x) for x in d['size']))[:5]
    res['requested'] = sorted(set(int(x) for x in d['requested']))[:5]
    res['clk_delay'] = pstats(d['clk_delay'][m])
    vals, cnt = np.unique(d['clk_delay'][m], return_counts=True)
    top = np.argsort(-cnt)[:6]
    res['clk_delay_hist'] = {int(vals[i]): int(cnt[i]) for i in top}
    # rate_diff convergence: first time |rate_diff_10s_mean - final| < 2 ppm
    final = ppm_dac
    n10 = int(10 * FS / q)
    cs = np.cumsum(np.insert((d['clk_rate_diff'] - 1) * 1e6, 0, 0))
    mv = (cs[n10:] - cs[:-n10]) / n10
    conv = np.where(np.abs(mv - final) < 2.0)[0]
    res['rate_diff_10s_mean_within_2ppm_after_s'] = float(tsec[conv[0] + n10]) if len(conv) else None
    rd_all = (d['clk_rate_diff'] - 1) * 1e6
    for a, b in ((0, 5), (5, 15), (15, 30), (30, 60), (60, 1e9)):
        mm = (tsec >= a) & (tsec < b)
        if mm.any():
            res[f'rate_diff_ppm_{a}-{int(min(b, tsec[-1]))}s'] = {'mean': float(rd_all[mm].mean()), 'pp': float(np.ptp(rd_all[mm])), 'std': float(rd_all[mm].std())}
    print(json.dumps(res, indent=1))
    json.dump(res, open(path.replace('.csv', '.analysis.json'), 'w'), indent=1)


if __name__ == '__main__':
    main()


def dll_faststart(t_ns, pos, bw_hz, zeta=math.sqrt(0.5), skip_s=0.0):
    """Same DLL, but the bandwidth starts at ~1 Hz and narrows as
    bw = max(bw_hz, 1/(2 t + 1)) so it locks fast without a long transient.
    Samples before skip_s are ignored (PipeWire's own DLL start-up)."""
    t = (t_ns - t_ns[0]).astype(float)
    p = (pos - pos[0]).astype(float)
    nspf = 1e9 / FS
    k0 = int(np.searchsorted(t, skip_s * 1e9))
    t_ref, p_ref = t[k0], p[k0]
    ppm = np.full(len(t), np.nan)
    for k in range(k0 + 1, len(t)):
        dn = p[k] - p_ref
        if dn <= 0:
            ppm[k] = ppm[k - 1]
            continue
        el = (t[k] - t[k0]) * 1e-9
        bw = max(bw_hz, 1.0 / (2.0 * el + 1.0))
        T = dn * nspf * 1e-9
        w = 2 * math.pi * bw * T
        pred = t_ref + dn * nspf
        e = t[k] - pred
        t_ref = pred + 2 * zeta * w * e
        p_ref = p[k]
        nspf += w * w * e / dn
        ppm[k] = (1e9 / FS / nspf - 1.0) * 1e6
    return ppm
