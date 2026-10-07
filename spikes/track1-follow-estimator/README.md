# Track 1 — follow estimator, open-loop study

Plain Python 3, no dependencies. It reproduces the arrival model measured from
real mpv into a pipe:
- 23.976 fps bursts;
- 50 ms ahead;
- 0–40 ms one-sided lateness;
- configurable drift and ramps.

It scores estimators by their position error and rate noise against the
truth.

- `model.py`: the arrival model and the evaluation (`evaluate`).
- `estimators.py`:
  - `HullDll`: the previous estimator, the hull edge into a DLL;
  - `QuantileTracker`: rejected;
  - `SplitEstimator`: candidate D, ported to
    `audio_sync::source::FollowEstimator`.
- `run*.py`: the sweeps behind plan §14 (`python3 run4.py`).
