# Arch/CachyOS packaging

PKGBUILDs for the Omniphony stack, split by component (unlike the Windows/macOS
bundles, which ship everything in one installer). The mpv package itself
(`mpv-omniphony`) lives in the separate `mpv-omniphony` repo (`packaging/`).

| Package            | Builds from                | License      | Installs |
|--------------------|----------------------------|--------------|----------|
| `orender`          | this repo (tag `v*`)       | GPL-3.0-only | `/usr/bin/orender`, `liborender.so*`, `orender.h`, `orender.pc`, layouts, the `omniphony-renderer` user service (not enabled) |
| `omniphony-studio` | this repo (tag `v*`)       | GPL-3.0-only | Omniphony Studio, the native egui/wgpu UI (depends on `orender`) |
| `harletty-bridge`  | sibling `harletty-bridge`  | Apache-2.0   | `/usr/lib/orender/libharletty_{dolby,dts,iamf}_bridge.so`, one library per codec family (up to harletty 0.8.x: one `libharletty_bridge.so`, removed on upgrade) |

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
  everywhere. The bridges are runtime `dlopen` plugins (the `*_bridge.so`
  pattern) that add compressed/object-audio decoding; without them PCM input
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
/usr/lib/orender/libharletty_dolby_bridge.so  # the decoder bridge plugins (optional),
/usr/lib/orender/libharletty_dts_bridge.so    # one per codec family
/usr/lib/orender/libharletty_iamf_bridge.so
/usr/bin/omniphony-studio-egui      # Studio UI (+ .desktop, icon)
/usr/bin/omniphony-studio           # → omniphony-studio-egui (the Tauri Studio's command name)
/usr/share/omniphony-studio-egui/   # its shipped files: layouts → ../orender/layouts, assets/
```

The engine auto-discovers the files `$ORENDER_BRIDGE_FILE` names (a path
list), else every `*_bridge.so` of the first of these folders that holds a
usable one: next to the host executable, `$ORENDER_BRIDGE_DIR`, the per-user
engine folder (`~/.local/share/omniphony/lib`), then `/usr/lib/orender`. The
packaged bridges are therefore found by mpv, the `orender` CLI and Studio's own
renderer with no configuration. To use other files, list them in
`render.bridge_paths` in `~/.config/omniphony/config.yaml` (shared by the CLI,
Studio and mpv), or on the mpv command line as a path list:

```
mpv --ad=orender \
    --ad-orender-bridge-path=/usr/lib/orender/libharletty_dolby_bridge.so:/usr/lib/orender/libharletty_dts_bridge.so \
    --ad-orender-config=/path/to/omniphony.yaml  film.mkv
```

A config that still names `/usr/lib/orender/libharletty_bridge.so` keeps
working for one release: the engine loads the family libraries of that folder
in its place.

## Building

These fetch pinned release tarballs — no checkout layout needed:

- `orender` 0.4.1 ← Omniphony `v0.4.1`.
- `omniphony-studio` 0.4.1 ← Omniphony `v0.4.1`.
- `harletty-bridge` ← harletty-bridge `v$pkgver`, plus the matching
  Omniphony `v$_omniver` source for its workspace path-deps
  (`bridge_api`/`spdif`/`sys`). From the release that splits it per codec
  family it builds and installs three plugins; the IAMF one links the
  system `opus`.

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
