#ifndef ORENDER_H
#define ORENDER_H

#pragma once

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * Opaque handle to a decode→render session. Created by [`orender_create`],
 * freed by [`orender_destroy`]. Internally a boxed [`Engine`].
 */
typedef struct OrenderRenderer {
  uint8_t _private[0];
} OrenderRenderer;

/**
 * Session configuration passed to [`orender_create`]. All `*const c_char`
 * fields are UTF-8, nul-terminated, and may be NULL (treated as "unset").
 */
typedef struct OrenderConfig {
  /**
   * Output/host sample rate in Hz. 0 → 48000.
   */
  uint32_t sample_rate;
  /**
   * Path to the omniphony YAML config (drives bridge path, speaker layout +
   * all render params). NULL → the shared default config used by the orender
   * CLI + studio (`~/.config/omniphony/config.yaml`).
   */
  const char *config_yaml_path;
  /**
   * Optional speaker-layout YAML path overriding the config. NULL → use the
   * config's embedded layout, else the 7.1.4 preset.
   */
  const char *speaker_layout_path;
  /**
   * Optional decoder bridge plugin path (the `*_bridge.so` produced by
   * the input format's bridge crate) overriding the config. NULL → taken
   * from the config YAML's `render.bridge_path` (the source of truth;
   * library hosts have no exe-relative search).
   */
  const char *bridge_path;
  /**
   * Codec identifier of the raw access units the host will feed (matches
   * the bridge's supported codec IDs, e.g. as used in FFmpeg/IEC958).
   * Disambiguates the bridge's raw transport (which carries no data-type
   * byte). NULL → the bridge sniffs the sync word.
   */
  const char *codec;
  /**
   * Enable the OSC live-control server. (Not yet wired in this build.)
   */
  int osc_enabled;
  /**
   * Incoming OSC port (0 = auto).
   */
  uint16_t osc_port_in;
  /**
   * Outgoing/monitoring OSC port.
   */
  uint16_t osc_port_out;
  /**
   * OSC bind address (default "127.0.0.1").
   */
  const char *osc_bind;
  /**
   * OSC monitoring target host.
   */
  const char *osc_host;
} OrenderConfig;

/**
 * Create a session. Returns NULL on failure (bad config, missing bridge, etc.).
 */
struct OrenderRenderer *orender_create(const struct OrenderConfig *cfg);

/**
 * Free a session created by [`orender_create`]. NULL is ignored.
 */
void orender_destroy(struct OrenderRenderer *r);

/**
 * 1 if the current presentation carries spatial objects, 0 if it is a plain
 * multichannel stream (the host should fall back to its standard decoder),
 * <0 on error. Meaningful after at least one [`orender_process`] call.
 */
int orender_is_spatial(const struct OrenderRenderer *r);

/**
 * Number of output channels (speakers) the renderer produces, 0 on error.
 */
uint32_t orender_channel_count(const struct OrenderRenderer *r);

/**
 * Write the active output layout's per-channel labels (one [`RChannelLabel`]
 * byte per speaker, in render order) so the host can build a channel map.
 *
 * Returns the channel count `N`. If `out_labels` is non-NULL and `cap >= N`,
 * the first `N` bytes are filled with label discriminants; otherwise nothing is
 * written — call with `out_labels = NULL` to query `N`, size a buffer, then
 * call again. Each byte is an `RChannelLabel` value (255 = Unknown). Returns 0
 * on error/NULL handle.
 */
uint32_t orender_channel_layout(const struct OrenderRenderer *r, uint8_t *out_labels, uint32_t cap);

/**
 * Reset after a seek/discontinuity (flushes decoder + renderer state, keeps
 * live params).
 */
void orender_reset(struct OrenderRenderer *r);

/**
 * Push one raw encoded packet and render whatever frames it yields.
 *
 * The caller owns `out` (capacity `out_cap_samples` floats). On success the
 * rendered interleaved samples are written there and `*out_frames` /
 * `*out_channels` / `*out_pts_us` are set.
 *
 * Returns: 0 = OK (may be 0 frames — need more data), >0 = output buffer too
 * small (nothing written; retry with a larger buffer), <0 = error.
 */
int orender_process(struct OrenderRenderer *r,
                    const uint8_t *pkt,
                    uintptr_t pkt_len,
                    int64_t _pts_us,
                    float *out,
                    uintptr_t out_cap_samples,
                    uintptr_t *out_frames,
                    uint32_t *out_channels,
                    int64_t *out_pts_us);

/**
 * Render the spatial overlay for the given OSD resolution and copy the ASS
 * `osd-overlay` payload into `out` (UTF-8, not nul-terminated).
 *
 * This *is* the overlay redraw: each call rebuilds the scene and advances the
 * motion trails, so the host (the mpv Lua shim) must call it exactly once per
 * redraw — typically on a periodic timer and on OSD resize. It also marks the
 * overlay "active" so the engine starts feeding it (the engine does no overlay
 * work until the first pull).
 *
 * Returns the number of bytes the payload needs. If `out` is non-NULL and
 * `cap >= len`, the first `len` bytes are written; otherwise nothing is written
 * (the host should grow its buffer and skip this redraw — the next one fits).
 * A handful of KiB is always enough; the output is bounded. Returns 0 when the
 * overlay is disabled, the resolution is zero, or there is nothing to draw.
 *
 * Handle-less by design: the overlay is a process-global singleton, and the Lua
 * shim has no session handle (it `ffi.load`s this already-loaded library).
 */
uintptr_t orender_overlay_ass(uint32_t res_x, uint32_t res_y, uint8_t *out, uintptr_t cap);

/**
 * Enable or disable the overlay (host keybind / script message). Disabling also
 * makes the engine stop feeding it. `0` = off, non-zero = on.
 */
void orender_overlay_set_enabled(int enabled);

/**
 * ABI major version. A bump means a breaking change (new soname).
 */
uint32_t orender_version_major(void);

/**
 * ABI minor version (backwards-compatible additions).
 */
uint32_t orender_version_minor(void);

#endif  /* ORENDER_H */
