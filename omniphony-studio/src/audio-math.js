/**
 * Numeric audio-level conversions.
 *
 * Separate from the *formatting* helpers in `mute-solo.js`: `formatLinearAsDb`
 * returns a display string like `"-12.3 dB"`, which is why several call sites
 * had grown their own inline `20 * Math.log10(x)` — the existing helper could
 * not give them a number to compute with.
 *
 * Anything that needs the value rather than the label belongs here. (A
 * `dbToLinear` counterpart lived in `mute-solo.js` with no callers at all; it
 * is gone rather than moved.)
 */

/**
 * Amplitude ratio → dBFS.
 *
 * Zero and negative ratios have no logarithm, so they return `floorDb` rather
 * than `-Infinity`, which would saturate any scale it reached.
 */
export function linearToDb(value, floorDb = -100) {
  const v = Number(value);
  if (!Number.isFinite(v) || v <= 0) {
    return floorDb;
  }
  return 20 * Math.log10(v);
}
