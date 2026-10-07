import sys
from model import evaluate
from estimators import *
cands = {
 'A hull+dll0.01': lambda: HullDll(0.01),
 'B quantile': lambda: QuantileTracker(),
}
cases = [('0ppm',0,None),('+80',80,None),('+1000',1000,None),('-1000',-1000,None),('ramp0->500@100-160',0,(100,160,500))]
for name, f in cands.items():
    for cn, ppm, ramp in cases:
        r = evaluate(f, T=300, ppm=ppm, ramp=ramp)
        print(f"{name:18s} {cn:20s} bias {r['bias_ms']:+7.3f}ms  p99 {r['p99_ms']:6.3f}ms  rate pp {r['rate_pp']:7.1f}  max {r['rate_max']:7.1f}ppm")
