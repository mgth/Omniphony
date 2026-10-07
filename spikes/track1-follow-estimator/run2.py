import sys
from model import evaluate
from estimators import *
cases = [('0',0,None),('+80',80,None),('+1000',1000,None),('-1000',-1000,None),('ramp',0,(100,160,500))]
def show(name, f):
    out=[]
    for cn, ppm, ramp in cases:
        r = evaluate(f, T=300, ppm=ppm, ramp=ramp)
        out.append(f"{cn}:{r['p99_ms']:.2f}ms/{r['rate_pp']:.0f}pp/{r['rate_max']:.0f}max")
    print(f"{name:34s} " + "  ".join(out))
show('A hull+dll', lambda: HullDll(0.01))
for lw in (30, 60, 120):
  for sw in (4, 8):
    for rt in (10, 30):
      for ot in (1, 4):
        show(f'D long{lw} short{sw} rt{rt} ot{ot}', lambda lw=lw,sw=sw,rt=rt,ot=ot: SplitEstimator(lw, sw, rt, ot))
