# liborender C ABI contract

`orender_ffi` builds the engine's only stable C surface: `liborender.so.<major>`
(Linux) / `orender.dll` (Windows) / `liborender.dylib` (macOS), described by the
generated header `orender_ffi/include/orender.h`. Known consumers: mpv's
`ad_orender.c` decoder + `orender_overlay.c` overlay client, and the smoke test
`orender_ffi/examples/smoke.c`. The `orender` CLI does NOT use this ABI — it
links the engine as a Rust crate.

## Version numbers (who is who)

| Number | Where | Meaning |
|---|---|---|
| ABI major (`ORENDER_ABI_MAJOR`) | `orender_ffi/src/lib.rs`, `#define` in header, `orender_version_major()` | Breaking-change counter. Linux soname `liborender.so.<major>` derives from it (build.rs). |
| ABI minor (`ORENDER_ABI_MINOR`) | same | Additive-change counter. Logging/diagnostics only. |
| Crate version (`orender_ffi/Cargo.toml`) | crate, `orender_build_id()`, `liborender-v*` release tags, Arch `pkgver` | Package/release identity. Moves faster than the ABI pair. |
| Build fingerprint | `orender_build_id()`, `/omniphony/state/render/version` | git-describe + build time; identifies the exact build. |

The ABI pair and the crate version have different lifecycles on purpose: a
release with no header change bumps the crate version only.

## Change policy

- **Additive** (new exported function, new `orender_set_option` key, enum value
  **appended**): bump `ORENDER_ABI_MINOR`. Existing consumers keep working
  unchanged.
- **Breaking** (changing/removing a symbol or its semantics, touching a struct
  layout, reordering/removing enum values): bump `ORENDER_ABI_MAJOR`, reset
  minor to 0. The Linux soname follows automatically; Windows/macOS file names
  do not change — consumers there are protected only by the runtime check.

**`OrenderConfig` is frozen at ABI major 0.** It crosses the boundary by layout
with no size handshake. New knobs go through `orender_set_option` (post-create)
or the config YAML (create-time), never through new struct fields.

**`OrenderChannelLabel` is append-only** and must mirror
`bridge_api::RChannelLabel` exactly — a unit test in `orender_ffi` asserts
discriminant parity and breaks the build when `bridge_api` adds a variant.

## Consumer contract

At load time a consumer must:

1. Resolve `orender_version_major`/`orender_version_minor` first; reject the
   library if they are missing (pre-handshake build).
2. Reject the library if `orender_version_major() != ORENDER_ABI_MAJOR` it was
   compiled against.
3. Gate optional features on **symbol presence** (`dlsym`), not on the minor.
   The minor is for logs. This makes both skew directions degrade gracefully:
   an older library just lacks the newer optional symbols; a newer library
   keeps every old symbol working.
4. Log `orender_build_id()` (when present) and the path the library was loaded
   from.

Probing an `orender_set_option` key: a return of `-1` means "this build does
not know that key" — treat it as feature-unavailable, not as an error.

## Options

`orender_set_option` keys, in the order they were added:

| Key | Values | Since | Meaning |
|---|---|---|---|
| `decode_thread` | `on`, `off` (default), `live` (0.11) | 0.10 | Decode on a thread of its own, overlapping the render, so the two share the work across two cores. A packet's audio then comes back from a later `orender_process` call — one packet's per call, about 30 ms of audio behind, or one packet if that is longer; occasionally two while the queue shrinks, so size the buffer for two — or from `orender_drain`, so only a host that stamps its output from `orender_output_packet_pts` (or `out_pts_us`) and drains at end of stream should turn it on. `on`/`off` force it: switch them while nothing is in flight — right after `orender_create`, after `orender_reset`, or once `orender_drain` has returned 0 frames; turning it off with packets still on the thread returns -2. `live` hands the choice to the user's `render.decode_thread` option (config.yaml, Studio, OSC); the engine follows it at packet boundaries, and when it is turned off mid-stream the thread winds down a packet per call before decoding goes back inline. |

## Output timestamps

`orender_process` carries its `pts_us` argument (read since 0.11) with the
packet's audio. `orender_output_packet_pts` then gives the `pts_us` of the
packet whose audio the last `orender_process` or `orender_drain` call returned
(the first one's, when a call returns two), or 0 when it returned none. Inline
that is the packet just passed in; with `decode_thread` on it is an older one,
which is what a host that stamps its output with its input timestamps needs.

## End of stream

`orender_drain` renders what the engine still holds when the input ends: with
`decode_thread` on, the packets still on the thread. One packet's audio per
call, as `orender_process` returns it, so a buffer that fits one packet's audio
fits a drain too: call it until it returns 0 frames. It is not a reset, and not
a DSP/reverb-tail flush; call `orender_reset` on a seek. A short output buffer
returns 1 with zero frames and keeps the audio: call drain again with a larger
buffer before sending more input — `orender_process` refuses input until it has
been collected. `orender_reset` discards it.

## Bump checklist

1. Edit `ORENDER_ABI_MINOR` (or `MAJOR`) in `orender_ffi/src/lib.rs` and extend
   the changelog comment above it.
2. `cargo build -p orender_ffi` — regenerates `include/orender.h`; commit it.
3. If breaking: expect the soname to change; update packaging (`PKGBUILD`
   symlinks) and warn mpv-omniphony (bundled lib name changes).
4. `cargo test -p orender_ffi` + run `examples/smoke.c` (CI does both).
