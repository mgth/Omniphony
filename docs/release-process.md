# Release process

How to cut an Omniphony Studio release `vX.Y.Z` on `mgth/Omniphony`, from a
green `main` to a published GitHub release. Written against v0.5.0 and v0.5.1;
amend this file whenever a release teaches something new.

## Version tracks — one number does not fit all

| Component | Repo | Tag namespace | Version line |
|---|---|---|---|
| Omniphony Studio bundle (Tauri host) | `mgth/Omniphony` | `v*` (e.g. `v0.5.1`) | stack version |
| Omniphony Studio, native host (egui/wgpu) | `mgth/Omniphony` | same `v*` tag, one archive per platform on the same release | stack version (`omniphony-studio-egui/Cargo.toml`, `[workspace.package]`) |
| Standalone liborender | `mgth/Omniphony` | same `v*` tag, `liborender-vX.Y.Z-<platform>.zip` assets (its own `liborender-v*` releases stopped at 0.4.3) | stack version |
| mpv player bundle | assets on `mgth/Omniphony`, source in `mgth/mpv-omniphony` | `mpv-v*` | stack version |
| mpv fork branches | `mgth/mpv` | `orender-v*` | stack version (plain `v*` collides with upstream mpv's ancient tags) |
| harletty-bridge | `harletty/harletty-bridge` | `v*` | **its own line** (0.7.x) — never tag it with the stack number |

A patch release usually only involves the first track. Cut the others only when
their component actually changed.

One number, the **release version**, is carried by both Studios, `orender`,
`liborender` and every renderer crate except `bridge_api` (the plugin ABI
contract), `spdif` (required by version from the bridge repository) and the
OSC contract. Its source of truth is `omniphony-renderer/Cargo.toml`'s
`[workspace.package] version`; `scripts/release_version.py` moves and checks
every copy, and CI fails when one is left behind (#676).

Which player and which bridges go with a release is stated in two places kept
by the same script: the README's **compatibility table** (release, liborender
ABI, player tag, `bridge_api` series) and the **manifest**
`omniphony-vX.Y.Z-manifest.json` attached to every release.

## 1. Preconditions

- The integration tree (`workflows/integration/omniphony`) is clean and on
  `main`; no completed work is sitting uncommitted. If integration is parked
  on another branch (a concurrent session's WIP), do not disturb it: drive
  the release from a dedicated temporary worktree of `main` instead
  (`git worktree add <dir> origin/main`) — done that way at 0.5.2.
- `git fetch origin --tags` exits non-zero because the rolling `integration`
  and `mpv-integration` tags move on every integration build ("would clobber
  existing tag"). That rejection is harmless — the branches and release tags
  still fetch; don't `--force` tags just to silence it.
- CI is green on `main` (`ci.yml`: fmt, build, full test suite incl. doctests).
- The changes shipping in this release have been validated (the user listens
  live; audio-path changes need that sign-off). The promotion PR records it
  with the `listened` label (step 3).
- Decide whether the release also needs an `mpv-v*` player release (see the
  table above; it has its own steps below): the bump names the player tag the
  release ships with, so the choice is made before the bump, not after.

## 2. Version bump (PR to `main`)

Never push to `main` directly — open a PR. One command moves every copy of the
release version:

```sh
scripts/release_version.py set X.Y.Z --player mpv-vX.Y.Z
```

`--player` names the `mpv-v*` tag this release ships with: the new one when a
player release follows (step 7), otherwise leave it out and the previous tag
stays. The script rewrites, keeping their formatting:

- `omniphony-renderer/Cargo.toml` — `[workspace.package] version` (every
  renderer crate but `bridge_api`/`spdif` inherits it) and
  `[workspace.metadata.release] player`;
- `omniphony-renderer/omniphony_geometry/Cargo.toml` (spelled out: the Studios
  build it from outside the workspace);
- `omniphony-studio-egui/Cargo.toml` — `[workspace.package] version`, the
  native Studio's About box and what its update check compares against tags;
- `omniphony-studio/package.json`, `package-lock.json` (both of its fields),
  `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`;
- the entries of the repository's own crates in the three tracked lockfiles
  (`omniphony-renderer/`, `omniphony-studio/src-tauri/` — which also records
  the native Studio's core — and `omniphony-studio-egui/`). CI builds with
  `--locked` and rejects a stale entry within a minute (it did at 0.6.0);
- the README's compatibility table: a new first row for `vX.Y.Z`, read from the
  tree (liborender ABI from `orender_ffi/src/lib.rs`, `bridge_api` series from
  its crate).

It ends with `check --tag vX.Y.Z`, the same check the release guard runs.
Commit everything it touched, open the PR, merge once CI is green. `orender
--version` prints the release tag or the commit, and a tarball build (AUR)
now stamps the release version rather than a stale crate number.

## 3. Promote `main` → `release`

Open a PR with **base=`release`, head=`main`** and merge it as a **merge
commit — not squash**. `release.yml`'s guard job checks
`git merge-base --is-ancestor <tag SHA> origin/release`; a squash rewrites the
SHAs and the guard rejects the tag.

`release` takes pull requests only, and requires `build-and-test`,
`build-macos`, `build-windows` and `listened`. `ci.yml` gates PRs to `release`
too, so the promotion PR re-runs the full suite it just ran on `main` — budget
for two CI passes (~6 min each at 0.5.1) between the bump merge and the tag.

**`listened`** (`.github/workflows/listened.yml`, rules in
`.github/scripts/listened-check.sh`): when the PR changes the render or output
path — the sources of the crates between the decoder and the device, the CLI,
the speaker layout presets; not OSC, tests or docs — the check fails until the
PR carries the `listened` label. CI cannot hear. The job summary lists the
commits on that path, which is what to listen to. Adding the label reruns only
this check. When new commits on the path reach the PR (main moved), the label
is removed with a comment, and the new head must be listened to again.

Back-merge discipline: a hotfix lands on `release` through its own PR (same
checks), and must then be merged back into `main`, or `main` regresses at the
next promotion.

## 4. Tag and build

```sh
git fetch origin release
git tag -a vX.Y.Z -m "Omniphony vX.Y.Z" origin/release
git push origin vX.Y.Z
```

The tag push triggers `release.yml`:

- **guard** — rejects tags whose SHA is not on `origin/release`, and runs
  `scripts/release_version.py check --tag`: the tag must be `vX.Y.Z` (or a
  `vX.Y.Z.N` polish tag) of the tree's release version, and the README's
  compatibility table must open with this release's row. A polish tag on a
  tree whose liborender ABI or `bridge_api` moved since the row was written
  is refused: that is a new release number, not a polish build.
- **build-studio** — Linux (`.deb`/`.rpm`/`.AppImage`), Windows
  (`.msi`/`.exe`), macOS arm64 (`.dmg`/`.app.tar.gz`, ad-hoc signed, not
  notarized). tauri-action creates a **draft** release named
  "Omniphony vX.Y.Z". Seven Tauri assets expected; whole run took ~17 min at
  0.5.1 (Linux is the slowest job at ~11 min).
- **native Studio**, same job, after tauri-action: builds
  `omniphony-studio-egui` on its pinned toolchain and attaches
  `omniphony-studio-egui-vX.Y.Z-{linux-x86_64.tar.gz,windows-x86_64.zip,macos-arm64.zip}`
  to the draft with `gh release upload` — the Studio, `orender` (built by its
  own step with the same command as the Tauri sidecar, so it finds that build
  done), `engine/` (the engine library), `layouts/`, `assets/` and the
  licence, in one directory. Three more assets. The Studio finds those files
  next to its executable (`core/src/host/bundle.rs`).
- **native Studio installers**, same job, before the archive (#677):
  `omniphony-studio-egui/scripts/package.sh` runs cargo-packager (configured
  in `omniphony-studio-egui/Cargo.toml`) and attaches
  `omniphony-studio-egui_X.Y.Z_amd64.deb`, `…_x86_64.AppImage`,
  `…_x64-setup.exe`, `…_x64_en-US.msi` and `…_aarch64.dmg` (the .app signed ad
  hoc, without the hardened runtime, as the Tauri bundle). Five more assets.
  Every form ships `orender` and the engine library; the Studio copies the
  library to `<local data>/omniphony/lib/` on startup for mpv
  (`core/src/host/engine_deploy.rs`), as the Tauri Studio does.
- **standalone liborender**, same job: the engine library (built by the
  same step as the native Studio's `orender`), with `orender.h`, as `liborender-vX.Y.Z-{linux-x86_64,windows-x86_64,macos-arm64}.zip`
  (flat, like the old `liborender-v*` archives). Three more assets.
- **manifest**, after the three builds: `omniphony-vX.Y.Z-manifest.json` —
  the README row as JSON, plus the commit. One more asset, **nineteen** in
  all.

The draft's URL is `releases/tag/untagged-<hash>` until it is published —
that is normal, not a broken tag association; it becomes `releases/tag/vX.Y.Z`
at publish.

The native Studio's release build (fat LTO, one codegen unit) adds six to
eight minutes per platform after tauri-action: the whole run took 19 min at
0.6.0, ten assets attached. Read the run's timestamps as UTC — a two-hour
"hang" at 0.6.0 was the local clock.

Expect a first-of-its-kind release build to expose latent build breakage that
PR CI never exercises: `--enable`-forced features that only auto-detect on the
runners (prefer `auto`), and any step that only runs on a tag push. Never cap
`apt-get install` timeouts (a slow mirror becomes a spurious failure — #276).

## 5. Notes and publish

- Write the notes in the session scratchpad, in the established style
  (see v0.5.0): `## Highlights` bullets, behaviour changes called out,
  `## Known limitations`, and the macOS quarantine/Gatekeeper install note
  (still needed until the app is notarized — #201).
- Notes span everything since the last **public** tag.
- Every release's notes (`v*`, `mpv-v*`, and the bridge's) open with the
  install line, so a user who lands on a release page finds the path to a
  film playing:

  ```markdown
  **New here?** Step-by-step install, from nothing to a film playing:
  [Linux](https://github.com/mgth/Omniphony/blob/main/docs/install/linux.md) ·
  [Windows](https://github.com/mgth/Omniphony/blob/main/docs/install/windows.md) ·
  [macOS](https://github.com/mgth/Omniphony/blob/main/docs/install/macos.md)
  ```

- Every Linux asset says what it runs on, in the notes and in the table of
  `docs/install/linux.md`: the build image and what the binary takes from the
  system. At 0.6.0: the native Studio and the Tauri bundles are built on
  Ubuntu 22.04 (glibc ≥ 2.35; the `orender` beside the Studio also needs a
  system PipeWire); the player zip links Ubuntu 24.04's FFmpeg and libplacebo
  and runs only there; the bridge is built on Ubuntu 24.04 and needs only glibc.
- After publishing, bump the asset names and release links in
  `docs/install/{linux,windows,macos}.md` on `main` (and the version pairing
  stated at the top of each page) together with the README download badges.

```sh
gh release edit vX.Y.Z --repo mgth/Omniphony --notes-file notes.md
gh release edit vX.Y.Z --repo mgth/Omniphony --draft=false --latest
```

Only the Studio bundle `v*` release is marked `--latest`; `mpv-v*` is
published not-latest.

To verify the latest marker, use `gh api repos/mgth/Omniphony/releases/latest`
— `gh release view --json` has no `isLatest` field.

## 6. Standalone liborender — no separate release any more

Until 0.6.0 liborender had its own `liborender-v*` tags and workflow, only
ever built when someone tagged it, and its line drifted from the Studio's
(`liborender-v0.4.3` without a `v0.4.3`). Its archives are now assets of every
`v*` release (step 4) under the release version; nothing to tag.

## 7. Optional: mpv-omniphony bundle release

Cut this whenever the player side changed: patches, launcher behaviour, or a
liborender ABI addition the player consumes (e.g. the PTS latency
compensation at 0.5.2). Source lives in `mgth/mpv-omniphony`; its tag build
publishes the bundles as a **draft on `mgth/Omniphony` under `mpv-vX.Y.Z`**
(via the `OMNIPHONY_RELEASE_TOKEN` secret).

1. The fork's `orender` branch (main tree `workflows/integration/mpv`, based
   on the pinned `v0.41.0`) carries everything to ship and is pushed.
2. In `mpv-omniphony`:
   - `scripts/regenerate-patches.sh <fork-path>` — regenerates `patches/`
     from `v0.41.0..orender`. (`patches-master/` is the separate series for
     the local Dolby Vision FEL build; it has its own regenerate script whose
     default base ref is stale — base it on the parent of the first fork
     commit — and it is NOT part of this release.)
   - `cp <fork>/audio/decode/ad_orender.c src/ad_orender.c` (kept in sync as
     the regenerate scripts remind).
   - Bump `OMNIPHONY_REF` in `.github/workflows/release.yml` to the new
     Omniphony tag — the build compiles liborender **from source at that
     ref**, so the Omniphony `vX.Y.Z` tag must exist before this repo's tag
     build runs.
   - If the liborender ABI gained symbols, extend
     `.github/scripts/stub-liborender.sh` (successor of the old
     mock-liborender): dlsym-optional symbols only degrade gracefully in the
     CI loader tests, but the stub should stay representative of the real
     surface.
   - `build-master.yml`, the master track's daily drift check, runs on the
     PR too and fails as soon as mpv master has moved under
     `patches-master/` — it had been red for four days before 0.6.0. Only
     `build-mpv` gates the stable bundle, so merge on that; but before the
     FEL beta tag, rebase the fork's `orender-master` onto
     `upstream/master`, regenerate `patches-master/` with that base
     (`scripts/regenerate-patches-master.sh <fork> <base>`) and merge it as
     its own PR — `v0.6.0-fel-beta.2` was tagged after it (beta.1 died on an unreachable download host, see below), and the
     AUR `mpv-omniphony-fel` pins the same base as `_mpvcommit`.
3. PR to its `main`; merge when its CI is green. Pushing workflow-file
   changes needs the SSH remote (`git@github.com-mgth:mgth/mpv-omniphony.git`);
   the HTTPS token lacks the `workflow` scope.
4. After the Omniphony tag exists, tag **from `origin/main`, never from a
   local `main` checkout**: a squash merge diverges any local `main`, and at
   0.5.2 a `git pull --ff-only` failure was swallowed by a `| tail` pipeline
   — the tag landed on the stale pre-PR commit and the build ran with the old
   `OMNIPHONY_REF` (cancel the run, delete the tag, retag). Use:
   `git fetch origin main && git tag vX.Y.Z origin/main && git push origin vX.Y.Z`.
   Publish the resulting `mpv-vX.Y.Z` draft on `mgth/Omniphony`
   **not-latest**, notes in the established style.
   - A single external fetch in a release job is a point of failure:
     at 0.6.0 `download.videolan.org` was unreachable for hours and the
     Windows job of both the bundle and the FEL beta died fetching
     libbluray. `scripts/build-libbluray-mingw.sh` now falls back to the
     Debian pool and Launchpad and checks the archive's hash. No release
     had been created for either tag, so `v0.6.0` was moved onto the fix
     and the beta retagged `-fel-beta.2`; the AUR packages built from
     those tags had to be re-summed.
5. Don't trust `gh run watch --exit-status` for the verdict — at 0.5.2 it
   returned success while `build-windows` had failed and `release` was
   skipped. Read `gh run view <id> --json conclusion,jobs` instead.
6. The Windows job's "Verify staged DLL imports resolve" step guards the
   ownstuff ffmpeg↔x265 pairing in both directions. It fired at 0.5.2
   because the old x265 4.1 pin outlived its reason (ffmpeg had been rebuilt
   against the current x265) — the pin is gone; if the pairing breaks again
   the fix is a new pin or its removal, per the step's message.
7. The local FEL build (`mpvo-fel`, `patches-master/`,
   `scripts/build-fel-local.sh`) is a dev-only artifact, never released;
   regenerate it locally whenever the fork's `orender` branch moves
   (`FEL_RENDERER_DIR` selects which renderer checkout provides the
   link-time liborender).

## 8. AUR packages — systematic, part of the release

Every release bumps the AUR packages before the release is considered done.
Local clones live in `aur/<pkg>/` at the workspace root (PKGBUILD sources of
truth: `packaging/arch/` in this repo, `packaging/` in mpv-omniphony).

| Package | Bump when |
|---|---|
| `orender` | every `v*` release |
| `omniphony-studio` | every `v*` release |
| `omniphony-studio-egui` | every `v*` release (from 0.6.0; template in `packaging/arch/omniphony-studio-egui`, depends on `orender` and links its layouts) |
| `mpv-omniphony` | when an `mpv-v*` bundle was cut: `_tag`, `depends=('orender>=X.Y.Z')` (the release-train couple) |
| `mpv-omniphony-fel` | with mpv-omniphony; `_tag` names the tag whose `patches-master/` apply to `_mpvcommit` — at 0.6.0 the FEL beta tag `v0.6.0-fel-beta.2` (the master-track rebase), with `pkgver=0.6.0` — and `_mpvcommit` the mpv master SHA those patches were rebased on and a build verified (`makepkg -fCd` itself, or `scripts/build-fel-local.sh`) |
| `harletty-bridge` | on its own line only (0.7.x, 0.8.x…) — never the stack number. Its `_omniver` names the Omniphony source tag the bridge's path-deps (`bridge_api`/`spdif`/`sys`) are taken from: the current Studio `v*` tag, fetched as the `v<_omniver>` archive (directory `Omniphony-<_omniver>`) — not a `liborender-v*` tag, which the PKGBUILD used to fetch and which no longer exists past 0.4.3 |

The templates in `packaging/arch/` are kept in step with the AUR clones (the
clones had drifted ahead — licence fix, engine resource — until 0.6.0 synced
them back). Per package: bump `pkgver` (+ `_tag`/pins), reset `pkgrel=1`, `updpkgsums`,
build-test with `makepkg -fCd` (`-d` because the runtime `orender` dep need
not be installed locally), `makepkg --printsrcinfo > .SRCINFO`, commit
`upgpkg: <pkg> X.Y.Z-1`, push `master`.

Judge each package by its `.pkg.tar.zst`, not by the exit status of a
`updpkgsums | tail` or `makepkg | tail` pipeline: the pipe returns `tail`'s
status, and at 0.6.0 a 404 on the source tarball went unnoticed that way
until the package list was checked.

Check the ssh agent holds the AUR key first
(`SSH_AUTH_SOCK=/run/user/1000/ssh-agent.socket ssh-add -l`) and only ask for
an `ssh-add` when it is empty. The AUR web site blocks robots (Anubis):
verify with `ssh aur@aur.archlinux.org list-repos` or the RPC API
(`aur.archlinux.org/rpc/v5/…`), never by scraping the site.

## 9. Post-release checks

- macOS: verify the signed bundle still decodes (the 0.5.0 hardened-runtime
  regression, #260/#261) — check `codesign -d --entitlements` on the shipped
  app and confirm the bridge `dlopen` works on a real machine.
- First download on macOS: Gatekeeper behaviour (#201).
- Amend **this document** with anything the release taught.
