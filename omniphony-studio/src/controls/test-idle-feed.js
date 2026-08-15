/**
 * Shared arming for the test idle feed.
 *
 * The renderer has one idle feed and one address for it, but two panels now want
 * it warm: the speaker test and the object test. Left to themselves each would
 * track its own armed flag and send `false` the moment *its* reason lapsed —
 * closing the speaker Test tab would disarm the feed out from under an open
 * object-injection panel. So the request is refcounted by key here, and only the
 * transitions between "nobody wants it" and "somebody does" reach the wire.
 *
 * The re-arm timer lives here too, for the same reason: the renderer expires the
 * arm after a keepalive window so a dead client cannot leave the feed running,
 * which means somebody has to keep saying so while any panel is open.
 */

import { invoke } from '@tauri-apps/api/core';

/** Well under the renderer's keepalive window, so a missed tick is harmless. */
const REARM_MS = 120_000;

const wanted = new Set();
let armed = false;
let timer = null;

function send(enable) {
  invoke('control_speaker_test_idle_feed', { enable }).catch(() => {});
}

function sync() {
  const want = wanted.size > 0;
  if (want === armed) return;
  armed = want;
  if (want) {
    send(true);
    timer = setInterval(() => send(true), REARM_MS);
  } else {
    clearInterval(timer);
    timer = null;
    send(false);
  }
}

/**
 * Declare whether `key` currently needs the output chain warm.
 * Idempotent: calling it repeatedly with the same value costs nothing.
 */
export function setIdleFeedRequest(key, want) {
  if (want) wanted.add(key);
  else wanted.delete(key);
  sync();
}

/** Drop every request and disarm — for page teardown. */
export function releaseIdleFeed() {
  wanted.clear();
  sync();
}
