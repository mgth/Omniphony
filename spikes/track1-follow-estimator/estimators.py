import math
from model import FS

class Dll:
    def __init__(s, bw=0.01, start=1.0, half=1.5, rate_fast=True, nominal=FS):
        s.bw, s.start, s.half, s.rate_fast, s.nominal = bw, start, half, rate_fast, nominal
        s.st = None
    def sched(s, age): return max(s.bw, s.start*0.5**(age/s.half))
    def observe(s, t, p):
        if s.st is None: s.st = [t, t, p, s.nominal]; return
        t0, tl, x, r = s.st; dt = t - tl
        if dt <= 0: return
        w = min(2*math.pi*s.sched(t-t0)*dt, 0.5)
        wr = w if s.rate_fast else min(2*math.pi*s.bw*dt, 0.5)
        pred = x + r*dt; e = p - pred
        s.st = [t0, t, pred + math.sqrt(2)*w*e, r + wr*wr*e/dt]
    def rate(s): return s.st[3] if s.st else None
    def position_at(s, t): return s.st[2] + s.st[3]*(t - s.st[1]) if s.st else None

class Hull:
    """Earliest-arrival LP fit (port of audio_sync::envelope)."""
    def __init__(s, bucket=0.25, window=30.0, nominal=FS):
        s.bucket, s.window, s.nominal = bucket, window, nominal
        s.origin=None; s.b=[]; s.open=None; s.fit=None
    def observe(s, t, recv):
        if s.origin is None: s.origin = t
        x = t - s.origin; y = recv - s.nominal*x; idx = math.floor(x/s.bucket)
        if s.open and s.open[0] == idx:
            if y > s.open[1][1]: s.open = (idx, (x, y))
            if not s.b: s.refit()
            return
        if s.open:
            s.b.append(s.open[1])
            while len(s.b) > 1 and s.open[1][0] - s.b[0][0] > s.window: s.b.pop(0)
        s.open = (idx, (x, y)); s.refit()
    def refit(s):
        pts = s.b + [s.open[1]]
        if len(pts) == 1: s.fit = (0.0, pts[0][0], pts[0][1]); return
        h = []
        for p in pts:
            while len(h) >= 2 and ((h[-1][0]-h[-2][0])*(p[1]-h[-2][1]) - (h[-1][1]-h[-2][1])*(p[0]-h[-2][0])) >= 0: h.pop()
            h.append(p)
        mx = sum(p[0] for p in pts)/len(pts)
        a, b = h[0], h[-1]
        for i in range(len(h)-1):
            if h[i][0] <= mx <= h[i+1][0]: a, b = h[i], h[i+1]; break
        sl = (b[1]-a[1])/(b[0]-a[0]) if b[0] > a[0] else 0.0
        s.fit = (sl, a[0], a[1])
    def rate(s): return s.nominal + s.fit[0] if s.fit else None
    def position_at(s, t):
        if not s.fit: return None
        sl, x0, y0 = s.fit; x = t - s.origin
        return s.nominal*x + y0 + sl*(x - x0)

class HullDll:
    """Candidate A (current Rust estimator)."""
    def __init__(s, bw=0.01): s.h = Hull(); s.d = Dll(bw=bw)
    def observe(s, t, recv):
        s.h.observe(t, recv); s.d.observe(t, s.h.position_at(t))
    def rate(s): return s.d.rate()
    def position_at(s, t): return s.d.position_at(t)

class QuantileTracker:
    """Candidate B: phase tracks a high quantile of the readings (moves up fast when a
    reading arrives earlier than predicted, down slowly otherwise); the rate is the hull
    slope, low-passed."""
    def __init__(s, tau=0.97, eta_start=200.0, eta_end=4.0, eta_half=3.0, rate_tau=10.0):
        s.h = Hull(); s.tau=tau; s.e0=eta_start; s.e1=eta_end; s.eh=eta_half; s.rt=rate_tau
        s.t0=None; s.t=None; s.p=None; s.r=FS
    def observe(s, t, recv):
        s.h.observe(t, recv)
        if s.t0 is None: s.t0 = s.t = t; s.p = recv; return
        dt = t - s.t
        if dt <= 0: return
        hr = s.h.rate()
        s.r += (hr - s.r)*(1 - math.exp(-dt/s.rt))
        pred = s.p + s.r*dt
        eta = max(s.e1, s.e0*0.5**((t-s.t0)/s.eh))
        q = s.tau if recv > pred else (s.tau - 1.0)
        s.p = pred + eta*q; s.t = t
    def rate(s): return s.r
    def position_at(s, t): return s.p + s.r*(t - s.t) if s.p is not None else None

class SplitEstimator:
    """Candidate D: rate from a long-window hull slope (low-passed), offset from the
    earliest arrival of the last few seconds against that rate (low-passed)."""
    def __init__(s, long_w=60.0, short_w=6.0, rate_tau=20.0, off_tau=2.0):
        s.h = Hull(window=long_w); s.sw=short_w; s.rt=rate_tau; s.ot=off_tau
        s.buf=[]; s.t_ref=None; s.r=FS; s.o=None; s.t=None
    def observe(s, t, recv):
        s.h.observe(t, recv)
        if s.t_ref is None: s.t_ref = t
        s.buf.append((t, recv))
        while s.buf and t - s.buf[0][0] > s.sw: s.buf.pop(0)
        dt = 0.0 if s.t is None else t - s.t
        if s.t is not None and dt <= 0: return
        hr = s.h.rate()
        # Trust the hull slope only once its window has some length.
        span = s.h.b[-1][0] - s.h.b[0][0] if len(s.h.b) > 1 else 0.0
        if span >= 5.0:
            # The time constant grows with the evidence: the hull slope's own
            # noise falls as its span grows, so early on it is followed
            # closely, later only through `rate_tau`.
            tau = min(s.rt, max(0.5, span/getattr(s, 'span_div', 4.0)))
            a = 1 - math.exp(-dt/tau) if s.t is not None else 1.0
            s.r += (hr - s.r)*a
        o_now = max(rv - s.r*(tt - s.t_ref) for tt, rv in s.buf)
        if s.o is None: s.o = o_now
        else:
            # The offset is the earliest arrival: rises at once, relaxes down slowly.
            s.o = o_now if o_now > s.o else s.o + (o_now - s.o)*(1 - math.exp(-dt/s.ot))
        s.t = t
    def rate(s): return s.r
    def position_at(s, t): return s.o + s.r*(t - s.t_ref) if s.o is not None else None
