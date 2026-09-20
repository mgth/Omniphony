/**
 * Channel spelling normalisation and alias-map building for the fixed-channel
 * editor. The accepted spellings themselves are published by the renderer in
 * the fixed-channel catalogue (each entry carries its 'aliases' field), which is
 * built from the single source of truth in omniphony-renderer/bridge_api/src/
 * labels.rs — so Studio can no longer drift from the layout matcher. This module
 * only normalises names and turns catalogue entries into a lookup map; it has no
 * runtime dependencies so it stays testable outside the app.
 */

// Normalise a channel name exactly like bridge_api::labels: drop whitespace,
// '_' and '-', then uppercase — "Top Front Left", "top_front-left" and "TFL"
// all become "TOPFRONTLEFT". Returns '' for anything that is not a string.
export function normalizeChannelName(name) {
  if (typeof name !== 'string') return '';
  return name.replace(/[\s_-]/g, '').toUpperCase();
}

// Build a normalised-spelling → canonical-label map from the renderer-published
// fixed-channel catalogue. Each entry contributes its own label plus every
// spelling in its 'aliases' list (already normalised upstream). Returns an
// empty map when no catalogue is available yet; callers fall back to their
// own defaults then.
export function buildChannelAliasMap(catalog) {
  const bySpelling = new Map();
  for (const entry of Array.isArray(catalog) ? catalog : []) {
    const label = typeof entry?.label === 'string' ? entry.label.trim() : '';
    if (!label) continue;
    bySpelling.set(normalizeChannelName(label), label);
    const aliases = Array.isArray(entry.aliases) ? entry.aliases : [];
    for (const alias of aliases) {
      const norm = normalizeChannelName(alias);
      if (norm) bySpelling.set(norm, label);
    }
  }
  return bySpelling;
}
