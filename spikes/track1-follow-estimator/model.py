"""Open-loop study of follow-source estimators on the measured mpv arrival model."""
import math, random

FS = 48000.0
FPS = 24000/1001
PER = FS/FPS            # frames per video frame
AHEAD = 0.050
JIT = 0.040
CB = 1024/48000         # observation cadence (output callbacks)

def arrivals(T, ppm, ramp=None, seed=1):
    """Yield observations (t_obs, received) as the servo sees them: the latest arrival
    at each callback, only when fresh. ramp=(t0, t1, ppm_end) linearly changes the rate."""
    rnd = random.Random(seed)
    # source clock: produced(t) integrates rate
    def rate(t):
        p = ppm
        if ramp:
            t0, t1, p1 = ramp
            if t >= t1: p = p1
            elif t > t0: p = ppm + (p1-ppm)*(t-t0)/(t1-t0)
        return FS*(1+p*1e-6)
    # build arrival list
    arr = []; produced = 0.0; t = 0.0; dt = 0.001; j = 0
    while t < T:
        produced += rate(t)*dt; t += dt
        while produced >= j*PER:
            arr.append((t + rnd.random()*JIT, j*PER + AHEAD*FS)); j += 1
    arr.sort()
    obs = []; k = 0; last = None; tc = 0.0
    truth = []  # (t, true earliest-arrival line value at t) : produced(t)+ahead
    produced = 0.0; t = 0.0; idx = 0
    # recompute produced at callback times
    prod_at = {}
    while tc < T:
        while idx < len(arr) and arr[idx][0] <= tc: last = arr[idx]; idx += 1
        if last and (not obs or last[0] > obs[-1][0]):
            obs.append((tc, last[0], last[1]))
        tc += CB
    return obs, rate

def true_line(rate, T, step=0.001):
    # cumulative produced frames at time t (+ahead) sampled
    xs=[0.0]; ys=[AHEAD*FS]; t=0.0; p=0.0
    while t < T:
        p += rate(t)*step; t += step; xs.append(t); ys.append(p + AHEAD*FS)
    return xs, ys

def interp(xs, ys, t):
    i = min(int(t/0.001), len(xs)-2)
    a = (t - xs[i])/0.001
    return ys[i] + a*(ys[i+1]-ys[i])

def evaluate(est_factory, T=300, ppm=0.0, ramp=None, seed=1, start=40.0):
    obs, rate = arrivals(T, ppm, ramp, seed)
    xs, ys = true_line(rate, T)
    est = est_factory()
    errs = []; rerr = []
    for (tc, t_arr, recv) in obs:
        est.observe(t_arr, recv)
        p = est.position_at(tc); r = est.rate()
        if p is None or tc < start: continue
        errs.append((tc, p - interp(xs, ys, tc)))
        rerr.append((tc, (r/rate(tc) - 1)*1e6))
    if not errs: return None
    vals = sorted(e for _, e in errs); med = vals[len(vals)//2]
    dev = sorted(abs(e-med) for _, e in errs)
    p99 = dev[int(0.99*(len(dev)-1))]/FS*1e3
    # rate error p-p per 10 s window and max abs
    pp = 0; w0 = rerr[0][0]; lo = hi = rerr[0][1]
    for t, v in rerr:
        if t - w0 > 10: pp = max(pp, hi-lo); w0 = t; lo = hi = v
        lo = min(lo, v); hi = max(hi, v)
    pp = max(pp, hi-lo)
    mx = max(abs(v) for _, v in rerr)
    return dict(bias_ms=med/FS*1e3, p99_ms=p99, rate_pp=pp, rate_max=mx)
