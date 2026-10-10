# Arch/CachyOS packaging

PKGBUILDs for the Omniphony stack, split by component (unlike the Windows/macOS
bundles, which ship everything in one installer). The mpv package itself
(`mpv-omniphony`) lives in the separate `mpv-omniphony` repo (`packaging/`).

| Package            | Builds from                | License      | Installs |
|--------------------|----------------------------|--------------|----------|
| `orender`          | this repo (tag `v*`)       | GPL-3.0-only | `/usr/bin/orender`, `liborender.so*`, `orender.h`, `orender.pc`, layouts, the `omniphony-renderer` user service (not enabled) |
| `omniphony-studio` | this repo (tag `v*`)       | GPL-3.0-only | Omniphony Studio, the native egui/wgpu UI (depends on `orender`) |
| `harletty-bridge`  | sibling `harletty-bridge`  | Apache-2.0   | `/usr/lib/orender/libharletty_bridge.so` |

Dependency shape:

- `omniphony-studio` and `mpv-omniphony` **depend on `orender`** (Studio
  finds the system binary next to its own executable — `/usr/bin/orender` —
  then via `which orender`; mpv links `liborender.so`). Studio also reads the
  layouts `orender` installs, through a link under its own share directory.
- Up to 0.6.0, `omniphony-studio` built the web-based Tauri Studio and the
  native one was a separate `omniphony-studio-egui` package. From 0.7.0 the
  native Studio is the only one: `omniphony-studio` builds it and
  `provides`/`conflicts`/`replaces` `omniphony-studio-egui`.
- `harletty-bridge` is a hard dependency of **nothing**: it is an `optdepends`
  everywhere. The bridge is a runtime `dlopen` plugin (the `*_bridge.so`
  pattern) that adds compressed/object-audio decoding; without it PCM input
  still renders. It is packaged separately (different repo, different license).

## Install layout

```
/usr/bin/orender                    # CLI renderer
/usr/lib/liborender.so.0.4.1        # real cdylib (DT_SONAME = liborender.so.0)
/usr/lib/liborender.so.0            # → liborender.so.0.4.1
/usr/lib/liborender.so              # → liborender.so.0      (dev/link symlink)
/usr/include/orender.h
/usr/lib/pkgconfig/orender.pc
/usr/share/orender/layouts/**/*.yaml   # virtual-bed fallback looks here
/usr/lib/systemd/user/omniphony-renderer.service  # not enabled: systemctl --user enable --now omniphony-renderer
/usr/lib/orender/libharletty_bridge.so # the decoder bridge plugin (optional)
/usr/bin/omniphony-studio-egui      # Studio UI (+ .desktop, icon)
/usr/bin/omniphony-studio           # → omniphony-studio-egui (the Tauri Studio's command name)
/usr/share/omniphony-studio-egui/   # its shipped files: layouts → ../orender/layouts, assets/
```

The engine auto-discovers the file `$ORENDER_BRIDGE_FILE` names, else a
`*_bridge.so` next to the host executable, then in `$ORENDER_BRIDGE_DIR`, then in the per-user engine folder
(`~/.local/share/omniphony/lib`), then in `/usr/lib/orender`, so the packaged
bridge is found by mpv, the `orender` CLI and Studio's own renderer with no
configuration. To use another file, name it in `render.bridge_path` in
`~/.config/omniphony/config.yaml` (shared by the CLI, Studio and mpv) or on the
mpv command line:

```
mpv --ad=orender \
    --ad-orender-bridge-path=/usr/lib/orender/libharletty_bridge.so \
    --ad-orender-config=/path/to/omniphony.yaml  film.mkv
```

## Building

These fetch pinned release tarballs — no checkout layout needed:

- `orender` 0.4.1 ← Omniphony `v0.4.1`.
- `omniphony-studio` 0.4.1 ← Omniphony `v0.4.1`.
- `harletty-bridge` 0.7.1 ← harletty-bridge `v0.7.1`, plus the matching
  Omniphony `v0.4.1` source for its workspace path-deps
  (`bridge_api`/`spdif`/`sys`).

No cross-package build order is required (nothing hard-depends on the bridge;
Studio needs `orender` **installed** to run, not to build):

```sh
cd orender          && makepkg -si
cd ../harletty-bridge && makepkg -si   # optional but recommended
cd ../omniphony-studio && makepkg -si
```

Then the mpv package from the separate `mpv-omniphony` repo (depends on
`orender>=0.4.1`).

**Bumping a release:** retag the source repo(s), then refresh the version vars
(`pkgver`, and `_omniver` in the bridge) and `sha256sums` (`updpkgsums` or
`makepkg -g`).

### Clean-room build

All build cleanly from the fetched tarballs (no `$srcdir` leakage thanks to
`--remap-path-prefix`). For a fully isolated build before publishing, use a
chroot:

```sh
makechrootpkg -c -r "$CHROOT"   # run in each package dir
```

## Verify

```sh
PKG_CONFIG_PATH=<pkgdir>/usr/lib/pkgconfig pkg-config --cflags --libs orender
# -> -I<...>/usr/include -L<...>/usr/lib -lorender
orender --help
```
