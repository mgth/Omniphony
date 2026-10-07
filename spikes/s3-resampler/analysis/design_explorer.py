#!/usr/bin/env python3
"""Analytic design explorer for the polyphase windowed-sinc fractional-delay FIR.

For a table of `L` phases x `T` taps (rows built from a Kaiser-windowed sinc,
each row normalised to unit DC gain, stored as f32) and an interpolation rule
between adjacent rows (linear, or cubic Hermite using stored d/dmu rows), this
computes, for every fractional position mu in a fine grid (including positions
between table rows):

    R_mu(w) = sum_k c_k(mu) * exp(j w (k - T/2 + 1 - mu))

i.e. the complex gain applied to a complex tone relative to the ideal
reconstruction x(p). R_mu = 1 is perfect.

* G(w) = mean_mu R_mu(w) is the LTI part (passband magnitude; it is real
  because the prototype is symmetric, so there is no residual delay).
* mean_mu |R_mu - G|^2 / |G|^2 is the time-varying part. At any ratio != 1
  mu sweeps uniformly, so this is the THD+N of a pure tone at w (the error
  lands as sidebands next to the tone). At ratio exactly 1, mu is constant
  and the output is LTI (no distortion at all).

Usage: OMP_NUM_THREADS=1 design_explorer.py [fs] [f_pass]
"""
import sys
import numpy as np


def kaiser(t, half, beta):
    x = np.clip(t / half, -1.0, 1.0)
    return np.i0(beta * np.sqrt(1.0 - x * x)) / np.i0(beta)


def rows(taps, beta, fc_norm, mu):
    """Normalised kernel rows (f64) for the fractional positions `mu`."""
    half = taps / 2.0
    k = np.arange(taps)[None, :]
    t = mu[:, None] + half - 1 - k  # p - j, in input frames
    h = fc_norm * np.sinc(fc_norm * t) * kaiser(t, half, beta)
    return h / h.sum(axis=1, keepdims=True)  # unit DC gain on every row


def build_table(taps, phases, beta, fc_norm, interp):
    mu = np.arange(phases + 1) / phases
    val = rows(taps, beta, fc_norm, mu)
    f32 = lambda a: a.astype(np.float32).astype(np.float64)
    if interp == "linear":
        return {"val": f32(val)}
    d = 1e-5
    der = (rows(taps, beta, fc_norm, mu + d) - rows(taps, beta, fc_norm, mu - d)) / (2 * d)
    der = der / phases  # derivative per table step
    if interp == "hermite":
        return {"val": f32(val), "der": f32(der)}
    raise ValueError(interp)


def kernels(table, mus, interp):
    val = table["val"]
    phases = val.shape[0] - 1
    pos = mus * phases
    m = np.minimum(np.floor(pos).astype(int), phases - 1)
    f = (pos - m).astype(np.float32).astype(np.float64)[:, None]
    a, b = val[m], val[m + 1]
    if interp == "linear":
        c = a + f * (b - a)
    else:
        da, db = table["der"][m], table["der"][m + 1]
        # cubic Hermite in Horner form: c0 + f(c1 + f(c2 + f c3))
        c0, c1 = a, da
        c2 = 3 * (b - a) - 2 * da - db
        c3 = 2 * (a - b) + da + db
        c = c0 + f * (c1 + f * (c2 + f * c3))
    return c.astype(np.float32).astype(np.float64)


def evaluate(taps, phases, beta, fc_norm, interp="linear", fs=48000.0, f_pass=20000.0, nmu=2048):
    freqs = np.linspace(10.0, f_pass, 400)
    table = build_table(taps, phases, beta, fc_norm, interp)
    mus = (np.arange(nmu) + 0.37) / nmu  # off-grid, between table rows
    c = kernels(table, mus, interp)
    w = 2 * np.pi * freqs / fs
    k = np.arange(taps)
    E = np.exp(1j * (k[:, None] - taps / 2 + 1) * w[None, :])
    R = (c @ E) * np.exp(-1j * mus[:, None] * w[None, :])
    G = R.mean(axis=0)
    err = np.mean(np.abs(R - G[None, :]) ** 2, axis=0) / np.abs(G) ** 2
    thdn_db = 10 * np.log10(err + 1e-300)
    mag_db = 20 * np.log10(np.abs(G))
    size = sum(v.size for v in table.values()) * 4
    return freqs, thdn_db, mag_db, size


def stopband(taps, beta, fc_norm, fs, f_stop):
    """Worst attenuation of the continuous prototype at or above f_stop."""
    half = taps / 2.0
    over = 64
    t = (np.arange(taps * over) - taps * over / 2) / over
    h = fc_norm * np.sinc(fc_norm * t) * kaiser(t, half, beta)
    n = 1 << 20
    H = np.abs(np.fft.rfft(h, n)) / over
    fr = np.fft.rfftfreq(n, 1.0 / (fs * over))
    sb = H[(fr >= f_stop) & (fr <= fs * 4)]
    return 20 * np.log10(sb.max() / H[0])


def main():
    fs = float(sys.argv[1]) if len(sys.argv) > 1 else 48000.0
    f_pass = float(sys.argv[2]) if len(sys.argv) > 2 else 20000.0
    f_stop = fs - f_pass
    fc_norm = 1.0  # transition centred on the input Nyquist
    print(f"fs={fs:.0f} pass<={f_pass:.0f} stop>={f_stop:.0f} fc=fs/2")
    print(f"{'T':>3} {'interp':>7} {'L':>5} {'beta':>5} | {'THD+N worst':>11} {'@10k':>7} {'@1k':>7}"
          f" | {'passband min/max dB':>20} | {'stopband':>8} | table KiB")
    grid = [
        (t, i, l, b)
        for t in (32, 48, 56, 64, 96)
        for b in (10.0, 12.0, 13.0, 14.0, 16.0)
        for i, ls in (("linear", (256, 512, 1024)), ("hermite", (32, 64, 128)))
        for l in ls
    ]
    if len(sys.argv) > 3:
        sel = [int(x) for x in sys.argv[3].split(",")]
        grid = [g for g in grid if g[0] in sel]
    for taps, interp, phases, beta in grid:
        freqs, thdn, mag, size = evaluate(taps, phases, beta, fc_norm, interp, fs, f_pass)
        i10 = np.argmin(abs(freqs - 10000))
        i1 = np.argmin(abs(freqs - 1000))
        sb = stopband(taps, beta, fc_norm, fs, f_stop)
        print(f"{taps:3d} {interp:>7} {phases:5d} {beta:5.1f} | {thdn.max():11.1f} {thdn[i10]:7.1f} {thdn[i1]:7.1f}"
              f" | {mag.min():+9.5f}/{mag.max():+9.5f} | {sb:8.1f} | {size/1024:7.1f}")


if __name__ == "__main__":
    main()
