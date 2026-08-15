/**
 * The injected test source's id, alone in its own module.
 *
 * It is needed by the source registry (`sources.js`), by mute/solo and by the
 * virtual-bed sweep — all of which the injection control itself imports from.
 * Declaring it there would make those imports circular, and a circular import
 * that happens to work is one that fails the day the evaluation order changes.
 * A module with no imports of its own cannot take part in a cycle.
 *
 * Deliberately not a number: the renderer's objects are numbered, the stale
 * purge in `tauri-bridge` only sweeps integer ids, and the objects list sorts
 * non-numeric ids last — so a name keeps this one out of the renderer's
 * numbering, safe from the sweep, and at the end of the list.
 */
export const OBJECT_TEST_SOURCE_ID = 'injection';
