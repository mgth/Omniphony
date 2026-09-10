# omniphony-studio-egui — phase 0 spike

A native egui/wgpu host for Omniphony Studio, built to answer the phase 0
questions of the frontend replacement study (10 September 2026): can egui host
the wgpu viewport at 60 fps under 100 Hz OSC updates, go idle when nothing
changes, stay small, render CJK labels and take IME input?

It is a spike, not a product. Nothing here is wired to control the renderer:
the option panel logs changes instead of sending them, so it can be pointed at
a live session without disturbing it.

## What it does

- Listens for the same OSC addresses as the Tauri Studio (`/omniphony/object/<id>/xyz|aed|meta`,
  the `/source/...` prototype forms, removals, heartbeat acks) and optionally
  registers with a renderer (`--register 127.0.0.1:9000`, read-only).
- Renders objects, speakers (from a `layouts/*.yaml` file) and the listener in a
  wgpu scene hosted by an `egui_wgpu::Callback`: own MSAA colour + depth
  target in `prepare`, composited into egui's pass in `paint`, no CPU copies.
- Floats fixed-extent panels over the viewport with internal scrolling, so
  expanding a section never changes the scene size.
- Generates its option widgets from a schema copied from the renderer's
  option registry (a toggle switch for `Bool`, a combo for `Enum`, a text
  field for `Str`).
- Draws labels as egui text projected from the 3D positions (CJK included when
  a CJK font is found on the system).
- Repaints only when OSC data arrives or the user interacts.

## Build and run

Requires Rust 1.95+ (`rust-toolchain.toml` pins 1.97.1 for this directory).

```bash
cd omniphony-studio-egui
cargo run --release -- --synthetic 64 --rate 100 --stats-interval 1
```

Useful flags: `--register host:port` (live renderer), `--listen-port N`,
`--layout ../layouts/9.1.6.yaml`, `--cjk-font /path/to/font.ttf`,
`--synthetic-stop-after 20` (feed stops, window must go idle).

## Gates and how to measure them

| Gate | Target | How |
|---|---|---|
| Frame rate under load | 60 fps with 64 objects at 100 Hz | `--synthetic 64 --rate 100 --stats-interval 1`, read `fps=` lines |
| Idle after a burst | 0 % process CPU once updates stop | `--synthetic 64 --synthetic-stop-after 20`, then sample `/proc/<pid>/stat` utime+stime over 5 s |
| Resident memory | under 150 MB | `VmRSS` in `/proc/<pid>/status`, or the Stats section |
| CJK labels | rendered, not boxes | the synthetic feed names half its objects in Japanese and Chinese |
| IME | composition works in a text field | type in the "Text: CJK and IME" field with an input method active |

Results are recorded in `docs/studio-native-ui-spike.md` once measured.
