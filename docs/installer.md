# RFC: one installer for player, engine and Studio

Status: **proposal** for the *Installer* part of #678. Nothing here is built.
The per-OS install pages (`docs/install/`, #719) describe the manual path and
stay.

## Problem

A working install today is three downloads from two repositories, unpacked by
hand into places that the lookup chains happen to search
(`docs/install/{linux,windows,macos}.md`):

| Piece | Asset | Released by |
|---|---|---|
| Player, with an engine inside | `mpv-omniphony-<tag>-<platform>.zip` (an AppImage on Linux from the next player release) | `mgth/mpv-omniphony`, published as `mpv-v*` on `mgth/Omniphony` |
| Decoder bridge | `harletty-bridge-<ver>-<platform>.zip` | `harletty/harletty-bridge`, its own version line |
| Studio, with `orender` and the engine | archive, or deb / AppImage / NSIS / MSI / dmg | `mgth/Omniphony` `v*` |

The install pages' failure sections are mostly about what an installer
would get right by construction: a bridge in the wrong folder, an engine and
a bridge from releases that do not pair, a stale engine in the per-user
folder winning over the one next to the player, the missing MSVC runtime on
Windows, and on Linux a player zip that runs on one distribution.

Parts already exist, each covering one piece:

| What | Where |
|---|---|
| Studio installers (deb, AppImage, NSIS with `installer-mode = "both"`, MSI, dmg), each carrying `orender` and the engine library | `omniphony-studio-egui/Cargo.toml` `[package.metadata.packager]`, `omniphony-studio-egui/scripts/package.sh`, `.github/workflows/release.yml` (cargo-packager 0.11.8) |
| Studio copies its engine to `<local data>/omniphony/lib/` on every start, when the bytes differ | `omniphony-studio-egui/core/src/host/engine_deploy.rs` |
| Arch packages: `orender` (`/usr/bin/orender`, `/usr/lib/liborender.so.0`), the bridge in `/usr/lib/orender/`, both Studios | `packaging/arch/*/PKGBUILD`, `packaging/arch/README.md` |
| Studio installs, starts, stops and removes the engine as a service: a systemd **user** unit on Linux, a Windows service through `New-Service` | `omniphony-studio-egui/core/src/host/commands/orender.rs:390-399`, `:516-626`; offered from `host/services/operations.rs:95-112` |
| `orender` runs under the Windows Service Control Manager and reports readiness to systemd (`Type=notify`) | `omniphony-renderer/src/main.rs:73-100`, `sys/src/lib.rs:43-71` |
| A machine-wide config on Windows, so a service and the user's processes read one file | `renderer/src/config.rs:1552-1600` |
| First start without a config: OSC on in the player's engine (#772), the bridge `mpv.conf` names lent to Studio's renderer (#770), a WASAPI fallback without an ASIO driver (#771), a connection panel that says what to do (#759) | `docs/persistence-policy.md` (exceptions), `core/src/host/mpv_bridge.rs`, `docs/install/windows.md` step 5 |
| A JACK service script for one NetJACK setup | `scripts/install-jackd-service.bat` |

What is missing is the piece that puts them together: one download per OS
that lays down a player, an engine, a bridge and Studio from releases that
pair, in the places the lookup chains search, and that can be removed again.

Three facts found while taking this inventory shape the design:

- **A stale per-user engine is never rejected.** mpv's loader tries the
  per-user copy before the one next to mpv and rejects a candidate only for
  another ABI *major* (`common/orender_dl.c` in the mpv fork `mgth/mpv`,
  branch `orender`, lines 4-21 and 246-275). The major is still 0 (`orender_ffi/include/orender.h:22`),
  so any older copy in `<local data>/omniphony/lib/` wins, and then pairs
  with whatever bridge sits next to it.
- **Studio's *Install service* writes the path of the `orender` it would
  launch** (`resolve_orender_binary`, `orender.rs:118-165`). Started from an
  AppImage that path is inside the AppImage's mount, which disappears when
  Studio exits; nothing there checks for it.
- **Nothing ships the MSVC runtime.** No `crt-static` flag in the tree, no
  redistributable in any installer. The Windows page lists the missing
  runtime as a failure of the player's engine; the same dependency applies to
  `orender.exe` and the Studio executable the NSIS and MSI installers lay
  down.

## Proposal

**A per-user installer for Windows and Linux, composed from the assets a
release already publishes, that puts the engine and the bridge into the
per-user engine folder `<local data>/omniphony/lib/`.** NSIS through
cargo-packager on Windows; a shell installer around the portable artifacts on
Linux. Running the engine as a service is an opt-in choice at install time
that installs the same unit or service Studio installs today. No config file
is written.

### Why the per-user engine folder

It is the one directory every lookup chain on the machine already searches
ahead of the copies that ship next to each binary:

- mpv's loader: `--ad-orender-library`, `$ORENDER_LIBRARY`, then this folder
  ("studio install"), then next to mpv, then the system loader
  (`orender_dl.c:4-15`).
- The engine's bridge discovery, for mpv and for Studio's `orender` alike:
  `--bridge-path`, `render.bridge_path`, `$ORENDER_BRIDGE_FILE`, next to the
  host executable, `$ORENDER_BRIDGE_DIR`, then this folder, then
  `/usr/lib/orender` on Unix (`orender_engine/src/bridge_loader.rs:256-292`,
  `:374-396`, `:463-475`).
- Studio's engine deploy writes the same file there (`engine_deploy.rs:7-14`),
  so a Studio of the same release finds identical bytes and copies nothing.

Writing the engine there also replaces the stale copy that would otherwise
win, which a copy next to the player cannot do. And it needs no elevation,
so the installer can be per-user, as everything else in the stack is except
the Windows config.

### Where each file goes

**Windows, per user (default):**

| Path | Content |
|---|---|
| `%LOCALAPPDATA%\Programs\Omniphony\player\` | the player folder as the `mpv-v*` zip has it (`mpv.exe`, `mpv.com`, its DLLs) |
| `%LOCALAPPDATA%\Programs\Omniphony\studio\` | the Studio archive's tree: `omniphony-studio-egui.exe`, `orender.exe`, `engine\`, `layouts\`, `assets\` (`core/src/host/bundle.rs` finds them next to the executable) |
| `%LOCALAPPDATA%\omniphony\lib\` | `orender.dll` from the release's `liborender-v*` archive, and the bridge (see *Decoder bridge*) |
| `%ProgramData%\omniphony\` | created empty, Users granted Modify; no `config.yaml` |
| Start menu | Studio, and the player started with `--ad=orender` |

Player and Studio stay in separate folders so the player's DLLs and the
Studio's binaries never shadow one another. The engine next to `mpv.exe`
(the player's own fallback) is left in place: the per-user copy is tried
first.

`%ProgramData%\omniphony` is the engine's config directory on Windows for
every account and for a service (`config.rs:1552-1560`). The engine creates
it on the first save (`config.rs:1304`), owned by whoever saved first; the
installer creates it ahead with the same grant Studio's service install
already applies (`icacls … /grant *S-1-5-32-545:(OI)(CI)M`, `orender.rs:584-587`),
so a second account or a service can write the file the first one created.

**Windows, per machine:** offered only to install the engine as a real
Windows service (below). Same layout under `%ProgramFiles%\Omniphony\`; the
engine goes next to `mpv.exe` and the bridge next to both `mpv.exe` and
`orender.exe`, because a service's `%LOCALAPPDATA%` is the system profile's.
A stale per-user engine of the installing account is replaced as in per-user
mode; other accounts' copies are not reachable, which is the residual case
listed in *Open questions*.

**Linux, per user:**

| Path | Content |
|---|---|
| `~/.local/lib/omniphony/mpv-omniphony.AppImage` | the player (the AppImage, not the zip: the zip links Ubuntu 24.04's FFmpeg sonames, `docs/install/linux.md`) |
| `~/.local/lib/omniphony/studio/` | the Studio archive's tree: `omniphony-studio-egui`, `orender`, `engine/`, `layouts/`, `assets/` |
| `~/.local/bin/` | links `mpv-omniphony`, `orender`, `omniphony-studio-egui` (not `mpv`: a distribution's mpv stays what `mpv` runs) |
| `$XDG_DATA_HOME/omniphony/lib/` (default `~/.local/share`) | `liborender.so.0` from the release, and the bridge |
| `~/.local/share/applications/` | desktop entries for Studio and for the player with `--ad=orender` |
| `~/.local/lib/omniphony/installed.txt` | every path written, for upgrade and uninstall |

The player AppImage carries its own fallback engine, which cannot be replaced
inside the image (`scripts/build-appimage.sh` in mpv-omniphony); the per-user
copy is tried first. `orender` is the plain binary from the Studio archive, at
a stable path a unit can name.

A Linux bundle runs where all three prebuilts run: the Studio archive needs
glibc 2.35 and PipeWire 0.3.65, the player AppImage glibc 2.38, the bridge
prebuilt glibc 2.39 (`docs/install/linux.md`, table *Which distribution each
prebuilt runs on*). The bridge sets the floor.

**macOS:** out of scope (see *Open questions*). The per-user folder exists
there too (`~/Library/Application Support/omniphony/lib`), so the same layout
would apply.

### Decoder bridge

The bridge is built and released from `harletty/harletty-bridge`, on its own
version line. It loads only in an engine built against the same `bridge_api`
minor (`omniphony-renderer/BRIDGE_API.md`, *Versioning*); the release
manifest states that series (`scripts/release_version.py:184-203`), and the
bridge's own releases do not publish which series they were built against.
`bridge_api` is 0.5.0 on `main` against 0.4.x at v0.6.0, so the next release
already needs a bridge release of its own.

Three ways to get it onto the machine. The choice is the maintainer's.

**A. Bundle a pinned matching bridge.** The installer workflow takes the
bridge release that matches the manifest's `bridge_api` series and puts it
next to the engine.
- Pairing: guaranteed when the installer is built, and the same on every
  machine.
- Release coupling: an installer cannot be cut until a matching bridge
  release exists. Needs the series of each bridge release in machine-readable
  form (a manifest asset on its releases, or a pin in this repository).
- Update path: a bridge fix reaches installer users through a new installer
  build (a `vX.Y.Z.N` rebuild suffices).
- Size: the bridge zip is about 1.2 MB, the Windows player zip about 74 MB.

**B. Download the matching bridge at install time.** The installer carries
the series and resolves the newest bridge release of it when it runs.
- Pairing: guaranteed at install time, provided the series mapping exists as
  in A.
- Release coupling: none at build time; an installer can ship before its
  bridge release, and then fails or skips the bridge until one exists.
- Update path: a re-run of the installer picks up a newer patch of the same
  series without a new installer.
- Cost: network access during install, a checksum the installer can check
  against, a failure mode when the release host is unreachable (the release
  process records one such outage, `docs/release-process.md` §7), and an
  offline install that has to fall back to C.

**C. Leave it as a separate install, and check it.** The installer lays down
player, engine and Studio. The bridge is installed on its own into the same
per-user folder, where every host finds it; the installer says where, and
whether a bridge is already there.
- Pairing: manual, as today; the most frequent failure on the install pages.
  An upgrade whose `bridge_api` minor moved leaves the old bridge refused
  until it is replaced.
- Release coupling: none.
- Update path: independent.
- Without a bridge the stack still runs: `orender` idles and reports the
  missing bridge over OSC (`tests/no_bridge_idle.rs`), Studio shows its
  *No decoder* banner, and the player plays the tracks the engine does not
  take as plain mpv.

A and B need a way to check a bridge's series without loading it in a player.
Step 4 of the plan adds it to `orender`, and C uses it for its check.

### Tooling

**Windows: NSIS through cargo-packager.** The release workflow already
installs cargo-packager 0.11.8 and builds the Studio's NSIS installer with it,
per user or per machine at the user's choice. The combined installer is the
same target fed a staged tree with three more pieces. It needs install steps
of its own (the `%ProgramData%` grant, the engine folder outside the install
directory, the service). cargo-packager's NSIS target takes a custom template;
whether that covers these steps is checked in step 3. If it does not, the
fallback is a hand-written `.nsi` built with the same `makensis`.

Not chosen:
- MSI (WiX, also built today for Studio): installs a service natively, but a
  per-user MSI cannot install one, writing outside the install directory needs
  custom actions, and authoring cost is higher for the same result.
- Inno Setup: comparable to NSIS for this job, but a second installer tool
  next to the one already in the release workflow.

**Linux: a shell installer around the portable artifacts.**
`omniphony-<tag>-linux-x86_64.tar.gz` holds the player AppImage, the Studio
archive tree, the release's `liborender.so.0`, the bridge (A), and
`install.sh` / `uninstall.sh`. No root, any distribution above the glibc
floor, a user unit when asked.

What the others cannot do well:
- AppImage: cannot write into the per-user engine folder or register a
  service; it can only run. Studio's AppImage stays, for Studio alone.
- deb: can ship a systemd user unit (in `/usr/lib/systemd/user/`, not
  enabled), but a deb holding the player is tied to the sonames of the
  distribution it was built on, as the player zip is today. The Studio deb
  stays, and gains the unit file (step 1).
- Distribution packages: the AUR already covers Arch; other distributions'
  packages are for their packagers.

### Running the engine as a service

What it is: a long-running `orender render <pipe> --continuous --enable-vbap
--osc … --osc-yield`, the command line Studio already uses for the renderer
it starts and for the service it installs (`orender.rs:169-231`). It
- renders what arrives on its input: the input pipe (`/tmp/orender.pipe` on
  Linux, `\\.\pipe\orender.input` on Windows, `orender.rs:85-101`), or the
  PipeWire sink input mode on Linux (`renderer/src/config.rs:547-557`);
- answers Studio on OSC port 9000 without Studio having to start it;
- steps aside for a player: an mpv whose engine has OSC on finds the port
  held and asks the holder to yield (`orender_engine/src/osc.rs:159-226`); a
  `--osc-yield` instance releases the OSC port and its audio output, idles,
  and takes both back when the player's engine sends `resume` on exit
  (`osc/dispatch.rs:239-266`, `src/cli/decode/session_run.rs:552-591`).

**Linux: a systemd user unit, not a system unit.** The config
(`~/.config/omniphony/config.yaml`), the per-user engine folder and the
PipeWire session are all the user's. The unit is the one Studio writes
(`orender.rs:390-399`: `Type=notify`, `Restart=on-failure`,
`WantedBy=default.target`), under the name Studio queries,
`omniphony-renderer.service` (`orender.rs:18`, `:441-480`), so Studio's
service panel shows it and Studio's auto-start watchdog stands aside for it
(`core/src/host/services/watchdog.rs:89-92`). `ExecStart` names
`~/.local/bin/orender`. It starts at login; at boot only with lingering
enabled, which the installer does not do.

**Windows: start at logon by default; a real service only per machine.**
- A per-user logon start (a `Run` key, or a scheduled task if its restart
  policy is wanted) runs `orender.exe` in the user's session: the same audio
  devices and drivers as mpv, the same per-user engine folder for the bridge,
  no elevation. Studio does not recognise it as a service (`orender.rs:483-500`
  queries the SCM only), but its watchdog still finds the port held and does
  not start a second renderer (`watchdog.rs:93-95`, `:166-168`).
- A Windows service starts before logon, under LocalSystem in session 0:
  `%LOCALAPPDATA%` is the system profile's, so the bridge must sit next to
  `orender.exe`; the config is shared through `%ProgramData%`, which was made
  machine-wide for this case. Audio output from session 0 is untested in this
  repository (`scripts/install-jackd-service.bat` exists because session 0
  is isolated from a user session's JACK server). Offered in per-machine mode
  only, with the command Studio uses (`orender.rs:572-600`), start type
  Manual unless the user asks for Automatic.

**Opt-in, unchecked by default.** Most users play films through the player,
whose engine is embedded and needs no separate process. A renderer that is
always running holds the OSC port and the input pipe, and the output device
while it renders. A player whose config turns OSC off never asks it to yield
(the yield happens only while binding the OSC port), so both would play. A
user who wants it ticks the box, or installs it later from Studio, which
writes the same unit.

### Config directory and first start

The installer writes **no `config.yaml`**.

- The persistence policy keeps render and engine settings out of
  `config.yaml` until Save (`docs/persistence-policy.md`, *The classes*). A
  seeded file chosen by the installer, such as headphones vs speakers, is a
  render setting written without a Save. It would need an exception in the
  policy, with its reason; this RFC does not propose one.
- A missing file already works: the player's engine turns OSC on
  (#772, *Exceptions* in the policy), renders to 7.1.4 by default, and
  Studio's first Save creates the file with OSC kept on
  (`Config::load_for_update`, `config.rs:981`).
- `%ProgramData%\omniphony\` is created on Windows for the permission reason
  above. On Linux nothing is created: the engine creates
  `~/.config/omniphony/` on its first write.
- `mpv.conf` is not touched. The shortcuts pass `--ad=orender`; Studio's
  *Activate in mpv config* switch remains the way to make it the default.
- If an existing `config.yaml` sets `render.bridge_path`, that path wins over
  every discovered bridge (`bridge_loader.rs:241-290`). The installer reads
  the file and warns when the path is not the bridge it installed; it does
  not edit it.

**MSVC runtime.** `orender.exe`, `orender.dll`, the Studio executable and the
bridge are built with the MSVC toolchain and link its C runtime dynamically.
A per-user installer cannot run Microsoft's redistributable installer, which
needs elevation. Two ways out:
- link the C runtime statically (`-C target-feature=+crt-static`) in this
  repository's Windows builds, and ask the bridge repository to do the same:
  no extra files, and every channel (zips, Studio installers, this one) is
  fixed at once. To check first: nothing allocated by one side of the
  `liborender` C ABI or the bridge ABI is freed by the other;
- ship the runtime DLLs app-local next to each executable that loads these
  libraries (`mpv.exe`, `orender.exe`).

Static linking is recommended (step 2).

### Upgrades

The installer always replaces all four pieces with those of one release.
`installed.txt` (Linux) and the NSIS uninstall log (Windows) say what an
earlier version wrote.

- **Running processes.** Windows refuses to overwrite a loaded DLL
  (`engine_deploy.rs:21-24` notes it for a running mpv). The installer stops
  the service or logon renderer it installed, and asks the user to close
  mpv and Studio. On Linux files are replaced by rename and running processes
  keep the old ones until restarted; the unit is restarted after the files.
- **Pairing.** Engine and bridge are written together (A, B), or the bridge
  is checked against the new series and a mismatch is reported (C).
- **Downgrade.** A config saved by a newer build carries a `schema_version`
  the older engine refuses to write (`docs/persistence-policy.md`); the
  installer does not need to guard it.

### Uninstall

Removes what it installed: the program folders, the engine and bridge in the
per-user engine folder (and Studio's deployed copy of the engine, which has
the same name), the shortcuts and desktop entries, the unit or service or
logon entry if it installed one (stopped and disabled first).

Never removes: `config.yaml`, its `.bak` and `config.live.yaml`;
`%ProgramData%\omniphony\`; Studio's own settings (`fr.omniphony.studio` under
`%APPDATA%` or `~/.config`, `core/src/host/startup.rs:52-69`); `mpv.conf`;
logs.

## Compatibility

- No change to any lookup order or default path: every location above is
  searched today. A user who sets `ORENDER_LIBRARY`, `--ad-orender-library`,
  `--bridge-path` or `render.bridge_path` keeps that choice.
- Manual installs keep working, and the install pages keep describing them.
- The Studio-only installers, the plain archives and the AUR packages are
  unchanged, apart from the unit file added to the deb and to `orender`.
- An installed service and Studio's *Install service* write the same unit
  name and contents, so either can replace the other's.

## Tests

- **Composition:** the installer workflow checks that the staged tree has
  every file at its path, as `package.sh` does for the Studio packages.
- **Windows (CI, `windows-latest`):** a silent per-user install (`/S`) into a
  fresh profile, then the install page's check
  (`mpv -v --no-config --ao=null --vo=null --length=1 av://lavfi:sine`) must
  log the engine from `%LOCALAPPDATA%\omniphony\lib` with `(studio install)`.
  The bridge discovery is checked with the reference bridge
  (`omniphony-renderer/reference_bridge`) standing in for the real one, built
  at the same tag. Then uninstall, and assert the config directory and a
  planted `config.yaml` are still there.
- **Linux (CI, containers):** `install.sh` into a temporary `HOME` on Ubuntu
  24.04 and Fedora, the same checks; `systemd-analyze --user verify` on the
  unit.
- **Unit text:** a test in the Studio core asserts that the shipped unit file
  equals `linux_service_unit` for the installed `orender` path, so the two
  never drift.
- **Clean Windows VM, by hand, per release:** first start with no
  redistributable installed (step 2), and the service option.

## Plan

Each step can be merged on its own and changes something for a user.

1. **Linux service, without an installer.** Ship
   `omniphony-renderer.service` (same text as Studio writes) as a file in the
   Studio deb (`/usr/lib/systemd/user/`) and in the AUR `orender` package,
   not enabled. Make Studio's *Install service* refuse, with a reason, an
   `orender` path inside an AppImage mount. Document `systemctl --user enable
   --now omniphony-renderer` in `docs/install/linux.md`. *User:* the engine
   runs at login with one command after installing the deb or the AUR
   package.
2. **No MSVC runtime needed on Windows.** Static C runtime in the Windows
   builds of `orender`, `liborender` and Studio, verified on a clean VM;
   the same request to the bridge repository. *User:* the Studio installers
   and the player zip start on a fresh Windows; the failure leaves the
   Windows page.
3. **Installer workflow and the Linux installer.** A workflow
   (`installer.yml`, manual or on publication of a `v*` release whose player
   tag exists) reads the manifest, downloads the published assets (Studio
   archive, `liborender` archive, the player of the manifest's `player` tag)
   and builds `omniphony-<tag>-linux-x86_64.tar.gz` with `install.sh`,
   `uninstall.sh` and an optional `--service`. Bridge per the decision on
   A/B/C; with C, none. It needs the player AppImage, which ships with the
   next player release. *User:* one download and one command on Linux.
4. **`orender` reports what it pairs with.** `orender --version` also prints
   the liborender ABI and the `bridge_api` series, and a check loads the
   discovered bridge the way a host would and exits non-zero with the reason.
   *User:* a mismatch is named before playback; the installer (C) and the
   install pages use it.
5. **Windows installer, per user.** The NSIS installer from the same staged
   tree: the layout above, the `%ProgramData%` grant, shortcuts, uninstall.
   Logon start as an unchecked option. *User:* one download on Windows, no
   elevation.
6. **Windows service, per machine.** The per-machine mode, offered only
   for the service, with the bridge next to `orender.exe`; manual test of
   audio output from session 0 with WASAPI and with an ASIO driver before it
   is offered. *User:* a renderer that runs before anyone logs in.
7. **Install pages.** Each page opens with the installer and keeps the
   manual path below it; release notes link the installer assets
   (`docs/release-process.md` §4-5).

Status: step 1 is #786. The packaged unit
is `packaging/systemd/omniphony-renderer.service`, checked against
`linux_service_unit` by a test in the Studio core.

Step 2 is the pull request that adds this paragraph. The static C
runtime is set for every MSVC target in `omniphony-renderer/.cargo/config.toml`
and `omniphony-studio-egui/.cargo/config.toml`; `cc` builds the ASIO SDK, Lua
and ring with `/MT` to match. Nothing crosses either ABI in a way the change
affects: the `liborender` C ABI hands out only an opaque handle freed by
`orender_destroy`, a static string, and caller-owned buffers; the bridge ABI's
`RVec`, `RString` and `RBox` free through the vtable of the side that
allocated them, and Rust's allocator on Windows is the process heap
(`HeapAlloc`), not the C runtime's. `scripts/check_windows_crt_imports.py`
fails the Windows CI job, the release and the integration build when
`orender.exe`, `orender.dll` or the Studio executable imports a runtime DLL.
Left: the clean-VM check by hand. The player's workflow builds `orender.dll`
with its own `RUSTFLAGS`, which replaces the config file
(mgth/mpv-omniphony#83 adds the flag there), and the bridge gets the same
change in harletty/harletty-bridge#134.

## Open questions

- **Bridge distribution:** A (bundled), B (downloaded at install) or C
  (separate, checked)? A and B also need the bridge releases to state their
  `bridge_api` series.
- **Service default on Windows:** is a logon start enough, or is a real
  service (before logon, session 0) wanted at all? If wanted, Manual or
  Automatic start?
- **Per-machine mode:** worth offering beyond the service? Other accounts'
  stale per-user engines would still win over the installed one. Making
  mpv's loader prefer the newer of the per-user copy and the one next to mpv
  would close that, as a player change.
- **macOS:** out of scope here. Without a Developer ID and notarization a
  `.pkg` meets the same Gatekeeper block as the zips (#201), and the per-user
  folder already gives macOS one place for engine and bridge. In scope once
  signing exists?
- **Signing:** the Windows builds are not code-signed
  (`docs/install/windows.md`, failure 4); an unsigned installer meets
  SmartScreen the same way. Ship before signing, or wait for it?
- **Linux floor:** the bridge prebuilt needs glibc 2.39, above the player
  AppImage's 2.38. Build the bridge on an older image so the bundle runs where
  the player does?
- **Speakers or headphones at install:** leave it to Studio's first run, or
  add a policy exception for an installer choice written to `config.yaml`?
