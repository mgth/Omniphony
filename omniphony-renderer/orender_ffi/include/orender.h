#ifndef ORENDER_H
#define ORENDER_H

#pragma once

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

// C-ABI major version of this library. A bump means a breaking change: the
// Linux soname `liborender.so.<major>` follows automatically (see build.rs);
// Windows/macOS consumers must gate on [`orender_version_major`] at load time
// (their library file name does not change).
//
// Exported into the generated header (as a `#define`) so a consumer can
// compare the constants it was compiled against with the runtime values
// reported by [`orender_version_major`]/[`orender_version_minor`]. Policy:
// additive change (new symbol, new [`orender_set_option`] key, enum value
// appended) bumps the minor; anything else (signature/struct/semantic change,
// symbol removal, enum reorder) bumps the major. See ABI.md.
#define ORENDER_ABI_MAJOR 0

// C-ABI minor version: backwards-compatible additions only. Consumers should
// gate optional features on symbol presence (dlsym), not on this value; it
// exists for logging and diagnostics.
#define ORENDER_ABI_MINOR 6

// Speaker-position labels written by [`orender_channel_layout`] and
// [`orender_bed_layout`] (one byte per channel). Mirrors the engine's
// ABI-stable `bridge_api::RChannelLabel` exactly (a unit test asserts
// discriminant parity); values are append-only per the ABI policy.
enum OrenderChannelLabel {
    OrenderChannelLabel_L = 0,
    OrenderChannelLabel_R = 1,
    OrenderChannelLabel_C = 2,
    OrenderChannelLabel_Lfe = 3,
    OrenderChannelLabel_Ls = 4,
    OrenderChannelLabel_Rs = 5,
    OrenderChannelLabel_Tfl = 6,
    OrenderChannelLabel_Tfr = 7,
    OrenderChannelLabel_Tsl = 8,
    OrenderChannelLabel_Tsr = 9,
    OrenderChannelLabel_Tbl = 10,
    OrenderChannelLabel_Tbr = 11,
    OrenderChannelLabel_Lsc = 12,
    OrenderChannelLabel_Rsc = 13,
    OrenderChannelLabel_Lb = 14,
    OrenderChannelLabel_Rb = 15,
    OrenderChannelLabel_Cb = 16,
    OrenderChannelLabel_Tc = 17,
    OrenderChannelLabel_Lsd = 18,
    OrenderChannelLabel_Rsd = 19,
    OrenderChannelLabel_Lw = 20,
    OrenderChannelLabel_Rw = 21,
    OrenderChannelLabel_Tfc = 22,
    OrenderChannelLabel_Lfe2 = 23,
    // The channel carries dynamic-object audio (position driven by metadata).
    OrenderChannelLabel_Object = 24,
    OrenderChannelLabel_Unknown = 255,
};
typedef uint8_t OrenderChannelLabel;

// Opaque handle to a decode→render session. Created by [`orender_create`],
// freed by [`orender_destroy`]. Internally a boxed [`Engine`].
typedef struct OrenderRenderer {
    uint8_t _private[0];
} OrenderRenderer;

// Session configuration passed to [`orender_create`]. All `*const c_char`
// fields are UTF-8, nul-terminated, and may be NULL (treated as "unset").
//
// **FROZEN at ABI major 0** — never add, remove, reorder, or retype fields:
// consumers compiled against an older header pass this struct by layout with
// no size handshake, so any change here is silently breaking. New knobs go
// through [`orender_set_option`] (post-create) or the config YAML
// (create-time). See ABI.md.
typedef struct OrenderConfig {
    // Output/host sample rate in Hz. 0 → 48000.
    uint32_t sample_rate;
    // Path to the omniphony YAML config (drives bridge path, speaker layout +
    // all render params). NULL → the shared default config used by the orender
    // CLI + studio (`~/.config/omniphony/config.yaml`).
    const char *config_yaml_path;
    // Optional speaker-layout YAML path overriding the config. NULL → use the
    // config's embedded layout, else the 7.1.4 preset.
    const char *speaker_layout_path;
    // Optional decoder bridge plugin path (the `*_bridge.so` produced by
    // the input format's bridge crate) overriding the config. NULL → taken
    // from the config YAML's `render.bridge_path` (the source of truth;
    // library hosts have no exe-relative search).
    const char *bridge_path;
    // Codec identifier of the raw access units the host will feed (matches
    // the bridge's supported codec IDs, e.g. as used in FFmpeg/IEC958).
    // Disambiguates the bridge's raw transport (which carries no data-type
    // byte). NULL → the bridge sniffs the sync word.
    const char *codec;
    // Enable the OSC live-control server. (Not yet wired in this build.)
    int osc_enabled;
    // Incoming OSC port (0 = auto).
    uint16_t osc_port_in;
    // Outgoing/monitoring OSC port.
    uint16_t osc_port_out;
    // OSC bind address (default "127.0.0.1").
    const char *osc_bind;
    // OSC monitoring target host.
    const char *osc_host;
} OrenderConfig;

// Create a session. Returns NULL on failure (bad config, missing bridge, etc.).
struct OrenderRenderer *orender_create(const struct OrenderConfig *cfg);

// Free a session created by [`orender_create`]. NULL is ignored.
void orender_destroy(struct OrenderRenderer *r);

// 1 while the current presentation carries dynamic objects, 0 while it is a
// plain multichannel stream, <0 on error.
//
// A live, observable fact about the stream (`docs/channel-object-contract.md`):
// it may flip in either direction mid-stream and must not be latched. Before
// the first decoded frame it reports the bridge's container-level guess.
// Hosts keep object-bearing tracks on the renderer regardless of the channel
// mode (a host cannot render objects); channel-based content follows
// [`orender_channel_mode`].
int orender_has_objects(const struct OrenderRenderer *r);

// Deprecated alias of [`orender_has_objects`], kept for hosts compiled
// against ABI minor < 6. Same values, same live semantics.
int orender_is_spatial(const struct OrenderRenderer *r);

// Dynamic object count of the last rendered frame (decoded channels minus the
// bed channels) for object-based content, `0` for plain multichannel, `-1` on
// a NULL handle / error. For the host's track info display. Meaningful after at
// least one [`orender_process`] call.
int orender_object_count(const struct OrenderRenderer *r);

// Dialogue normalisation level in dBFS (always ≤ 0) once the stream has
// declared it, or [`i32::MIN`] when unknown / not yet seen (also on a NULL
// handle / error). For the host's track info display.
int orender_dialnorm_db(const struct OrenderRenderer *r);

// Write the bed channel labels of the last object-based frame (one
// [`OrenderChannelLabel`] byte per bed channel) so the host can show the bed
// composition (e.g. "LFE+11 objects").
//
// Same query/fill convention as [`orender_channel_layout`]: returns the bed
// channel count `N`; if `out_labels` is non-NULL and `cap >= N`, the first `N`
// bytes are filled (else nothing is written — call with `out_labels = NULL` to
// query `N`). `0` for plain multichannel / no bed / NULL handle / error.
uint32_t orender_bed_layout(const struct OrenderRenderer *r, uint8_t *out_labels, uint32_t cap);

// Configured render mode for channel-based (non-object) content:
// 0 = host, 1 = spatial; <0 on error. When this is `host` (0) and
// [`orender_has_objects`] reports 0, the host should decline this track and fall
// back to its native decoder. Meaningful once the renderer is created (the mode
// comes from config / live params, not from the stream).
int orender_channel_mode(const struct OrenderRenderer *r);

// Override the channel render mode for non-object content at runtime (a
// per-host override of the config value): 0 = host, 1 = spatial. No-op on a
// NULL handle.
void orender_set_channel_mode(struct OrenderRenderer *r, int mode);

// Output channel mapping: 0 = by_index (positionless — output port N carries
// layout speaker N), 1 = by_name (positional — each channel tagged with its
// speaker position). <0 on error. The host uses this to choose between a
// positionless and a positional channel map.
int orender_channel_mapping(const struct OrenderRenderer *r);

// Override the output channel mapping at runtime: 0 = by_index, 1 = by_name.
// No-op on a NULL handle or an unknown code.
void orender_set_channel_mapping(struct OrenderRenderer *r, int mode);

// Number of output channels (speakers) the renderer produces, 0 on error.
uint32_t orender_channel_count(const struct OrenderRenderer *r);

// Write the active output layout's per-channel labels (one
// [`OrenderChannelLabel`] byte per speaker, in render order) so the host can
// build a channel map.
//
// Returns the channel count `N`. If `out_labels` is non-NULL and `cap >= N`,
// the first `N` bytes are filled with label discriminants; otherwise nothing is
// written — call with `out_labels = NULL` to query `N`, size a buffer, then
// call again. Each byte is an [`OrenderChannelLabel`] value (255 = Unknown).
// Returns 0 on error/NULL handle.
uint32_t orender_channel_layout(const struct OrenderRenderer *r, uint8_t *out_labels, uint32_t cap);

// Reset after a seek/discontinuity (flushes decoder + renderer state, keeps
// live params).
void orender_reset(struct OrenderRenderer *r);

// Push one raw encoded packet and render whatever frames it yields.
//
// The caller owns `out` (capacity `out_cap_samples` floats). On success the
// rendered interleaved samples are written there and `*out_frames` /
// `*out_channels` / `*out_pts_us` are set.
//
// Returns: 0 = OK (may be 0 frames — need more data), >0 = output buffer too
// small (nothing written; retry with a larger buffer), <0 = error.
int orender_process(struct OrenderRenderer *r,
                    const uint8_t *pkt,
                    uintptr_t pkt_len,
                    int64_t _pts_us,
                    float *out,
                    uintptr_t out_cap_samples,
                    uintptr_t *out_frames,
                    uint32_t *out_channels,
                    int64_t *out_pts_us);

// Render the spatial overlay for the given OSD resolution and copy the ASS
// `osd-overlay` payload into `out` (UTF-8, not nul-terminated).
//
// This *is* the overlay redraw: each call rebuilds the scene and advances the
// motion trails, so the host (the mpv Lua shim) must call it exactly once per
// redraw — typically on a periodic timer and on OSD resize. It also marks the
// overlay "active" so the engine starts feeding it (the engine does no overlay
// work until the first pull).
//
// Returns the number of bytes the payload needs. If `out` is non-NULL and
// `cap >= len`, the first `len` bytes are written; otherwise nothing is written
// (the host should grow its buffer and skip this redraw — the next one fits).
// A handful of KiB is always enough; the output is bounded. Returns 0 when the
// overlay is disabled, the resolution is zero, or there is nothing to draw.
//
// Handle-less by design: the overlay is a process-global singleton, and the Lua
// shim has no session handle (it `ffi.load`s this already-loaded library).
uintptr_t orender_overlay_ass(uint32_t res_x, uint32_t res_y, uint8_t *out, uintptr_t cap);

// Enable or disable the overlay (host keybind / script message). Disabling also
// makes the engine stop feeding it. `0` = off, non-zero = on.
void orender_overlay_set_enabled(int enabled);

// Drop all overlay scene state (object positions, levels, trails, labels)
// without touching the master enable. Used by a host that stops feeding the
// overlay — e.g. mpv routing channel audio to its native decoder in host mode —
// so the spatial overlay clears immediately instead of lingering on the last
// frame until the trails decay. The next pull after feeding resumes shows the
// live scene again; the user's overlay on/off preference is preserved.
void orender_overlay_clear(void);

// Suppress or resume *all* overlay drawing — the wireframe cube included — for a
// live session, independent of the master enable. A host that keeps the engine
// alive but is not spatial-rendering (mpv in host mode, decoding channel audio
// natively) sets `0` so the whole overlay disappears, and `1` when it resumes
// spatial rendering. `0` = not rendering (blank), non-zero = rendering.
void orender_overlay_set_rendering(int rendering);

// Flip the master enable and return the new state (1 = on, 0 = off).
int orender_overlay_toggle(void);

// Flip object-label visibility and return the new state (1 = on, 0 = off).
int orender_overlay_toggle_labels(void);

// Flip object visibility (markers + labels + trails + depth lines) and return
// the new state (1 = on, 0 = off).
int orender_overlay_toggle_objects(void);

// Flip whether motion trails are drawn and return the new state (1 = on,
// 0 = off). Clears the trail buffers when disabling.
int orender_overlay_toggle_trails(void);

// Flip the object energy heatmap and return the new state (1 = on, 0 = off).
int orender_overlay_toggle_heatmap(void);

// Advance the heatmap colour gradient to the next index (wraps 0..=4) and return
// the new index.
uint32_t orender_overlay_cycle_heatmap_colormap(void);

// Step the heatmap depth-plane count by `delta` (clamped to 1..=12) and return
// the new count.
uint32_t orender_overlay_adjust_heatmap_bands(int32_t delta);

// Render the object energy heatmap as a single flattened BGRA bitmap
// (premultiplied alpha) for mpv's `overlay-add`, drawn *under* the ASS overlay.
//
// On success copies `w*h*4` BGRA bytes into `out` and writes the geometry into
// `geom` (6 × i32: `[x, y, w, h, dw, dh]` — top-left position, source size, and
// the on-screen display size mpv scales the source to), then returns the number
// of bytes written. Returns 0 — and writes nothing — when the overlay is
// disabled, the resolution is zero, the buffers are too small, or there is no
// audible object. The bitmap is bounded (`FIELD_BITMAP_MAX²·4` ≈ 256 KiB).
//
// Read-only with respect to the scene: unlike `orender_overlay_ass`, this does
// not advance trails or the pull clock (the ASS pull already does), so the host
// may call it alongside the ASS redraw.
uintptr_t orender_overlay_heatmap_bgra(uint32_t res_x,
                                       uint32_t res_y,
                                       uint8_t *out,
                                       uintptr_t cap,
                                       int32_t *geom);

// ABI major version of the loaded library (see [`ORENDER_ABI_MAJOR`]). A
// consumer must refuse a library whose major differs from the one it was
// compiled against.
uint32_t orender_version_major(void);

// ABI minor version of the loaded library (backwards-compatible additions;
// see [`ORENDER_ABI_MINOR`]). For logging — gate features on symbol presence.
uint32_t orender_version_minor(void);

// Human-readable build identifier of the loaded library:
// `"<crate-version> <git-describe> (built <timestamp>)"`. Static storage,
// never NULL — for host logs, so "which engine did I actually load" is one
// log line instead of a debugging session.
const char *orender_build_id(void);

// Set a named runtime option on a session — the additive evolution path for
// the frozen [`OrenderConfig`]: new knobs get a string key here instead of a
// struct field, so consumers compiled against older headers keep working and
// newer consumers can probe.
//
// Returns 0 on success, -1 for an unknown key (also how a consumer probes
// whether this build supports a key), -2 for an invalid value, -3 on a NULL
// handle/argument or internal error.
//
// No keys are defined at ABI 0.5 — every call returns -1. The mechanism ships
// ahead of the first key so consumers can adopt the probe pattern now.
int orender_set_option(struct OrenderRenderer *r, const char *key, const char *value);

#endif  /* ORENDER_H */
