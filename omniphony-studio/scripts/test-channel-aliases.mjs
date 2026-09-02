/**
 * Assert the channel-spelling mirror of bridge_api::labels (the renderer's
 * single source of truth for channel naming) against its normalisation rules.

 * `src/channel-aliases.js` only normalises names and builds a lookup map from
 * the catalogue the renderer publishes; the accepted spellings themselves ride
 * in that payload. What can drift is the *normalisation*, so this pins it to
 * the Rust side's rules (drop whitespace/`_`/`, then uppercase) with vectors
 * from bridge_api/src/labels.rs tests.

 * Run with `npm run test:labels`.
 */

import assert from 'node:assert';

import { buildChannelAliasMap, normalizeChannelName } from '../src/channel-aliases.js';

// --- normalisation parity (mirrors bridge_api::labels `normalise`) ---------
assert.equal(normalizeChannelName("Top Front Left"), "TOPFRONTLEFT");
assert.equal(normalizeChannelName("top_front-left"), "TOPFRONTLEFT");
assert.equal(normalizeChannelName("tfl"), "TFL");
assert.equal(normalizeChannelName("height-right"), "HEIGHTRIGHT");
assert.equal(normalizeChannelName("LFE2"), "LFE2");
// Non-strings and blank input normalise to '' (unknown, not an error).
assert.equal(normalizeChannelName(null), '');
assert.equal(normalizeChannelName(42), '');
assert.equal(normalizeChannelName('   '), '');

// Offline contract (pinned by inspection — virtual-bed.js is not importable
// from plain node because of its Tauri/i18n imports): with no catalogue
// published, canonicalChannelName falls back to the fallback bed, so exact
// canonical spellings still resolve through this same normalisation:
//   canonicalChannelName('L')   === 'L'    (fallback-bed name 'L')
//   canonicalChannelName('tfl') === 'TFL'  (case/separator-normalised)
assert.equal(normalizeChannelName('L'), 'L');
assert.equal(normalizeChannelName('tfl'), 'TFL');

// --- alias map from a renderer-shaped catalogue ---------------------------
const catalog = [
  { label: "L", aliases: ["FL", "L", "FRONTLEFT", "LEFTFRONT"] },
  { label: "C", aliases: ["C", "FC", "CENTER", "CENTRE", "FRONTCENTER"] },
  { label: "TFL", aliases: ["TFL", "TPFL", "TOPFRONTLEFT", "UFL", "LTF", "LEFTTOPFRONT", "HEIGHTLEFT", "HL"] },
];
const map = buildChannelAliasMap(catalog);
// The map is keyed by normalised spellings and callers (canonicalChannelName)
// normalise before looking up — so do the same here.
const resolve = (spelling) => map.get(normalizeChannelName(spelling));
assert.equal(resolve("FL"), "L");
assert.equal(resolve("frontleft"), "L");
assert.equal(resolve("top front left"), "TFL"); // spaces stripped by normalisation
assert.equal(resolve("height-left"), "TFL"); // "-" and "_" stripped too
assert.equal(resolve("top_front_left"), "TFL");
assert.equal(resolve("frontcenter"), "C"); // only resolvable via the published table

// Empty or malformed input yields an empty map without throwing.
assert.equal(buildChannelAliasMap(null).size, 0);
assert.equal(buildChannelAliasMap([]).size, 0);
assert.equal(buildChannelAliasMap([{ label: "" }, { aliases: ["ORPHAN"] }]).size, 0);

console.log("channel-aliases: all assertions passed");
