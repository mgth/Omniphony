# Release process

How to cut an Omniphony Studio release `vX.Y.Z` on `mgth/Omniphony`, from a
green `main` to a published GitHub release. Written against v0.5.0 and v0.5.1;
amend this file whenever a release teaches something new.

## Version tracks — one number does not fit all

| Component | Repo | Tag namespace | Version line |
|---|---|---|---|
| Omniphony Studio (egui/wgpu, `omniphony-studio-egui/`) | `mgth/Omniphony` | `v*` (e.g. `v0.7.0`): installers plus one archive per platform | stack version (`omniphony-studio-egui/Cargo.toml`, `[workspace.package]`) |
| Standalone liborender | `mgth/Omniphony` | same `v*` tag, `liborender-vX.Y.Z-<platform>.zip` assets (its own `liborender-v*` releases stopped at 0.4.3) | stack version |
| mpv player bundle | assets on `mgth/Omniphony`, source in `mgth/mpv-omniphony` | `mpv-v*` | stack version |
| mpv fork branches | `mgth/mpv` | `orender-v*` | stack version (plain `v*` collides with upstream mpv's ancient tags) |
| harletty-bridge | `harletty/harletty-bridge` | `v*` | **its own line** (0.7.x) — never tag it with the stack number |

A patch release usually only involves the first track. Cut the others only when
their component actually changed.

**0.7.0 drops the web Studio.** Up to 0.6.0 a release shipped two Studios: the
web-based Tauri host (the `omniphony-studio` tree, assets `Omniphony.Studio_…`) and the
native one. From 0.7.0 the native Studio is the only one (#677): the Tauri
tree, its CI jobs and lockfiles are gone, the release's Studio assets are the
native installers and archives (`omniphony-studio-egui_…`,
`omniphony-studio-egui-v…`), and the AUR `omniphony-studio` package installs
the native Studio. Say so in the 0.7.0 notes (step 5).

One number, the **release version**, is carried by Studio, `orender`,
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
- `omniphony-renderer/omniphony_geometry/Cargo.toml` (spelled out: the Studio
  builds it from outside the workspace);
- `omniphony-studio-egui/Cargo.toml` — `[workspace.package] version`, the
  Studio's About box and what its update check compares against tags;
- the entries of the repository's own crates in the two tracked lockfiles
  (`omniphony-renderer/` and `omniphony-studio-egui/`). CI builds with
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
this check, which records in a comment the head the label was added on. Every
later run compares that head with the current one: when commits since then
touch the path (main moved), the label is removed with a comment, and the new
head must be listened to again.

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
- **draft** — creates the **draft** release named "Omniphony vX.Y.Z" (once,
  before the three builds, so they do not race to create it; a rerun reuses
  it). Its body is a placeholder until step 5. (Up to 0.6.0 tauri-action
  created it.)
- **build-studio**, one job per platform (Linux x86_64, Windows x86_64, macOS
  arm64): builds `omniphony-studio-egui` on its pinned toolchain, and
  `orender` and the engine library in `omniphony-renderer/`.
- **Studio installers**, same job (#677):
  `omniphony-studio-egui/scripts/package.sh` runs cargo-packager (configured
  in `omniphony-studio-egui/Cargo.toml`) and attaches
  `omniphony-studio-egui_X.Y.Z_amd64.deb`, `…_x86_64.AppImage`,
  `…_x64-setup.exe`, `…_x64_en-US.msi` and `…_aarch64.dmg` (the .app signed ad
  hoc, without the hardened runtime). Five assets. Every form ships `orender`
  and the engine library; the Studio copies the library to
  `<local data>/omniphony/lib/` on startup for mpv
  (`core/src/host/engine_deploy.rs`).
- **Studio archive**, same job, after the installers:
  `omniphony-studio-egui-vX.Y.Z-{linux-x86_64.tar.gz,windows-x86_64.zip,macos-arm64.zip}`
  attached to the draft with `gh release upload` — the Studio, `orender`,
  `engine/` (the engine library), `layouts/`, `assets/` and the licence, in
  one directory. Three more assets. The Studio finds those files next to its
  executable (`core/src/host/bundle.rs`).
- **standalone liborender**, same job: the engine library (built by the
  same step as the Studio's `orender`), with `orender.h`, as `liborender-vX.Y.Z-{linux-x86_64,windows-x86_64,macos-arm64}.zip`
  (flat, like the old `liborender-v*` archives). Three more assets.
- **manifest**, after the three builds: `omniphony-vX.Y.Z-manifest.json` —
  the README row as JSON, plus the commit. One more asset, **twelve** in
  all (nineteen up to 0.6.0, with the seven Tauri bundles).

The draft's URL is `releases/tag/untagged-<hash>` until it is published —
that is normal, not a broken tag association; it becomes `releases/tag/vX.Y.Z`
at publish.

The Studio's release build (fat LTO, one codegen unit) takes six to eight
minutes per platform: the whole run, with the Tauri bundles still in it, took
19 min at 0.6.0. Read the run's timestamps as UTC — a two-hour
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
  system. At 0.6.0: the Studio (native and, until 0.7.0, Tauri) is built on
  Ubuntu 22.04 (glibc ≥ 2.35; the `orender` beside the Studio also needs a
  system PipeWire); the player zip links Ubuntu 24.04's FFmpeg and libplacebo
  and runs only there; the bridge is built on Ubuntu 24.04 and needs only glibc.
  From 0.7.0: the player also ships an AppImage (built on Ubuntu 24.04, glibc
  ≥ 2.38, its own libraries inside), and the bridge's IAMF library needs the
  system's `libopus.so.0`.
- After publishing, bump the asset names and release links in
  `docs/install/{linux,windows,macos}.md` on `main` (and the version pairing
  stated at the top of each page) together with the README download badges:
  the player badge at every `mpv-v*` release, the FEL badge only when a FEL
  beta was cut.
  0.7.0 made the family bridges (`harletty_{dolby,dts,iamf}_bridge`,
  `docs/multi-bridge.md`) the pages' main text and kept the combined
  `harletty_bridge` as an "Upgrading from 0.6.0" note; drop that note at the
  release whose engine no longer reads the combined name as the family
  libraries beside it.

```sh
gh release edit vX.Y.Z --repo mgth/Omniphony --notes-file notes.md
gh release edit vX.Y.Z --repo mgth/Omniphony --draft=false --latest
```

Only the Studio `v*` release is marked `--latest`; `mpv-v*` is
published not-latest.

**The installers** (`docs/installer.md`). Publishing a release starts
`installer.yml`. It composes the combined installer from the published files
and attaches it to the `vX.Y.Z` release once both `vX.Y.Z` and the player
release its manifest names are out, so in practice when `mpv-vX.Y.Z` is
published (§7); the run started by `vX.Y.Z` alone ends on a notice. Check
that `omniphony-vX.Y.Z-linux-x86_64.tar.gz` and
`omniphony-vX.Y.Z-windows-x86_64-setup.exe` are on the release afterwards. To
compose it again, or for a release published before the workflow existed:

```sh
gh workflow run installer.yml --repo mgth/Omniphony -f tag=vX.Y.Z -f attach=true
```

`-f player_tag=…` takes the player from another release than the manifest's.
A Studio archive older than 0.7.0 has no `engine/` directory and cannot be
composed.

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

1. The mpv-side code lives in the fork `mgth/mpv` and nowhere else
   (mgth/mpv-omniphony#82): mpv-omniphony holds no patches and no copy of the
   decoder, its builds compile a fork tag named in `mpv-fork.env`. The fork's
   `orender` branch (main tree `workflows/integration/mpv`, based on the
   pinned `v0.41.0`) carries everything to ship and is pushed, and
   `orender-master` (the master track, for the FEL beta) carries the same
   commits.
   - If `orender_ffi/include/orender.h` changed since the last player
     release, copy the generated header over the fork's
     `common/orender_abi.h`, below its leading comment, on **both** branches.
     mpv-omniphony's CI diffs the two and the release build refuses a
     difference.
   - Tag both heads and push the tags. A fork tag is never moved: the first
     tags of a player version are `orender-vX.Y.Z` and
     `orender-master-vX.Y.Z`, later ones add `.1`, `.2`, ….
2. In `mpv-omniphony`:
   - Update the two tags and their commits in `mpv-fork.env` (and
     `MPV_MASTER_BASE` when `orender-master` was rebased). The scripts refuse
     a tag that does not resolve to the commit recorded next to it.
   - Bump `OMNIPHONY_REF` in `.github/workflows/release.yml` to the new
     Omniphony tag, and the tagged-beta ref in `build-master-beta.yml` with
     it — the build compiles liborender **from source at that ref**, so the
     Omniphony `vX.Y.Z` tag must exist before this repo's tag build runs.
     Until it does, the pull request's header check runs against `main`.
   - If the liborender ABI gained symbols, extend
     `.github/scripts/stub-liborender.sh` (successor of the old
     mock-liborender): dlsym-optional symbols only degrade gracefully in the
     CI loader tests, but the stub should stay representative of the real
     surface.
   - The `fork-glue` job of its CI replays the fork's delta as two patch
     series (ASIO, then orender) on the upstream base of each track, and
     fails on a commit that mixes the two or on a vendored header that has
     drifted. The series are attached to the release as
     `…-patches.tar.gz`; nothing is committed.
   - `build-master.yml`, the master track's daily drift check, merges the
     fork's master tag into mpv master HEAD and fails as soon as mpv master
     has moved under it — it had been red for four days before 0.6.0. Only
     `build-mpv` gates the stable bundle, so merge on that; but before the
     FEL beta tag, rebase the fork's `orender-master` onto
     `upstream/master`, tag it, and update `mpv-fork.env` with the new tag
     and `MPV_MASTER_BASE` as its own PR — `v0.6.0-fel-beta.2` was tagged
     after such a rebase (beta.1 died on an unreachable download host, see
     below).
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
7. The local FEL build (`mpvo-fel`, `scripts/build-fel-local.sh`) is a
   dev-only artifact, never released. It assembles its tree with
   mpv-omniphony's `scripts/mpv-source.sh master`, so it builds the fork's
   tagged master track; to build fork work that is not tagged yet, point it
   at the checkout (`MPV_FORK_URL=<fork> MPV_SOURCE_REF=orender-master`).
   `FEL_RENDERER_DIR` selects which renderer checkout provides the link-time
   liborender.

## 8. AUR packages — systematic, part of the release

Every release bumps the AUR packages before the release is considered done.
Local clones live in `aur/<pkg>/` at the workspace root (PKGBUILD sources of
truth: `packaging/arch/` in this repo, `packaging/` in mpv-omniphony; the
`harletty-bridge` recipe has no copy here and lives in its AUR clone only).

| Package | Bump when |
|---|---|
| `orender` | every `v*` release |
| `omniphony-studio` | every `v*` release (template in `packaging/arch/omniphony-studio`, depends on `orender` and links its layouts). From 0.7.0 it builds the native Studio and `provides`/`replaces` `omniphony-studio-egui`; up to 0.6.0 it built the Tauri Studio |
| `omniphony-studio-egui` | no longer bumped from 0.7.0: the native Studio's separate package (0.6.0 only), folded into `omniphony-studio`. Ask the AUR for a merge into `omniphony-studio` once that package ships 0.7.0 |
| `mpv-omniphony` | when an `mpv-v*` bundle was cut: `_tag`, `_forktag` (the `MPV_FORK_STABLE_TAG` of `mpv-fork.env` at that tag: the package builds the fork's tag archive, nothing is patched), `depends=('orender>=X.Y.Z')` (the release-train couple) |
| `mpv-omniphony-fel` | with mpv-omniphony; `_tag` names the mpv-omniphony tag (for `patches-libplacebo/` and the licence texts) and `_forktag` the fork's master-track tag at that tag (`MPV_FORK_MASTER_TAG`), whose archive replaces the mpv master snapshot plus `patches-master/` the package used up to 0.6.0; a build verified (`makepkg -fCd` itself, or `scripts/build-fel-local.sh`) |
| `harletty-bridge` | on its own line only (0.7.x, 0.8.x…) — never the stack number. Its recipe lives in `aur/harletty-bridge/` only, with no template in this repo. Its `_omniver` names the Omniphony source tag the bridge's path-deps (`bridge_api`/`spdif`/`sys`) are taken from: the current Studio `v*` tag, fetched as the `v<_omniver>` archive (directory `Omniphony-<_omniver>`) — not a `liborender-v*` tag, which the PKGBUILD used to fetch and which no longer exists past 0.4.3. From the bridge release with one library per codec family, it builds the `harletty-<family>-bridge` crates listed in `_families` (check them against that release's workspace) and installs `libharletty_{dolby,dts,iamf}_bridge.so`; pacman drops the old `libharletty_bridge.so` on upgrade, and `opus` is a dependency of the IAMF one |

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
