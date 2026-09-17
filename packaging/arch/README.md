# Arch/CachyOS packaging

PKGBUILDs for the Omniphony stack, split by component (unlike the Windows/macOS
bundles, which ship everything in one installer). The mpv package itself
(`mpv-omniphony`) lives in the separate `mpv-omniphony` repo (`packaging/`).

| Package            | Builds from                | License      | Installs |
|--------------------|----------------------------|--------------|----------|
| `orender`          | this repo (tag `v*`)       | GPL-3.0-only | `/usr/bin/orender`, `liborender.so*`, `orender.h`, `orender.pc`, layouts |
| `omniphony-studio` | this repo (tag `v*`)       | GPL-3.0-only | Studio UI, Tauri host (no bundled sidecar — depends on `orender`) |
| `omniphony-studio-egui` | this repo (tag `v*`)  | GPL-3.0-only | Studio UI, native egui/wgpu host (depends on `orender`) |
| `harletty-bridge`  | sibling `harletty-bridge`  | Apache-2.0   | `/usr/lib/orender/libharletty_bridge.so` |

Dependency shape:

- `omniphony-studio`, `omniphony-studio-egui` and `mpv-omniphony` **depend on
  `orender`** (either Studio finds the system binary next to its own
  executable — `/usr/bin/orender` — then via `which orender`; mpv links
  `liborender.so`). The native Studio also reads the layouts `orender`
  installs, through a link under its own share directory.
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
/usr/lib/orender/libharletty_bridge.so # the decoder bridge plugin (optional)
/usr/bin/omniphony-studio           # Studio UI, Tauri host (+ .desktop, icons, resources)
/usr/bin/omniphony-studio-egui      # Studio UI, native host (+ .desktop, icon)
/usr/share/omniphony-studio-egui/   # its shipped files: layouts → ../orender/layouts, assets/
```

The engine auto-discovers any `*_bridge.so` next to the host executable; system
hosts in `/usr/bin` won't find one there, so point them at the plugin
explicitly — either `render.bridge_path` in `~/.config/omniphony/config.yaml`
(shared by the CLI, Studio and mpv) or on the mpv command line:

```
mpv --ad=orender \
    --ad-orender-bridge-path=/usr/lib/orender/libharletty_bridge.so \
    --ad-orender-config=/path/to/omniphony.yaml  film.mkv
```

## Building

These fetch pinned release tarballs — no checkout layout needed:

- `orender` 0.4.1 ← Omniphony `v0.4.1` (same commit as `liborender-v0.4.1`).
- `omniphony-studio` 0.4.1 ← Omniphony `v0.4.1`.
- `harletty-bridge` 0.7.1 ← harletty-bridge `v0.7.1`, plus the matching
  Omniphony `liborender-v0.4.1` source for its workspace path-deps
  (`bridge_api`/`spdif`/`sys`).

No cross-package build order is required (nothing hard-depends on the bridge;
Studio needs `orender` **installed** to run, not to build):

```sh
cd orender          && makepkg -si
cd ../harletty-bridge && makepkg -si   # optional but recommended
cd ../omniphony-studio && makepkg -si
cd ../omniphony-studio-egui && makepkg -si   # the native Studio, same dependency shape
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
