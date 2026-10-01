from model import evaluate
from estimators import *
cases = [('0',0,None),('+80',80,None),('+1000',1000,None),('-1000',-1000,None),('ramp',0,(100,160,500))]
def show(name, f, seeds=(1,2)):
    out=[]
    for cn, ppm, ramp in cases:
        rs=[evaluate(f, T=400, ppm=ppm, ramp=ramp, seed=s) for s in seeds]
        out.append(f"{cn}:{max(r['p99_ms'] for r in rs):.2f}/{max(r['rate_pp'] for r in rs):.0f}/{max(r['rate_max'] for r in rs):.0f}")
    print(f"{name:30s} " + "  ".join(out), flush=True)
for lw in (60, 120):
  for sw in (15, 30):
    for rt in (10, 20):
      for ot in (1, 3):
        show(f'D L{lw} S{sw} rt{rt} ot{ot}', lambda lw=lw,sw=sw,rt=rt,ot=ot: SplitEstimator(lw, sw, rt, ot))
