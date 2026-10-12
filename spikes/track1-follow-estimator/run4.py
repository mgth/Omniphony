from model import evaluate
from estimators import *
cases = [('0',0,None),('+80',80,None),('+1000',1000,None),('-1000',-1000,None),('ramp',0,(100,160,500))]
def show(name, f, seeds=(1,2,3), start=40.0):
    out=[]
    for cn, ppm, ramp in cases:
        rs=[evaluate(f, T=400, ppm=ppm, ramp=ramp, seed=s, start=start) for s in seeds]
        out.append(f"{cn}:{max(r['p99_ms'] for r in rs):.2f}/{max(r['rate_pp'] for r in rs):.0f}/{max(r['rate_max'] for r in rs):.0f}")
    print(f"{name:30s} " + "  ".join(out), flush=True)
for div in (2.0, 4.0, 8.0):
    for rt in (10, 20):
        def f(div=div, rt=rt):
            e = SplitEstimator(120, 30, rt, 1); e.span_div = div; return e
        show(f'D L120 S30 rt{rt} div{div}', f)
        show(f'   (from 20 s)', f, start=20.0)
