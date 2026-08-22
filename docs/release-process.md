# Release process

How to cut an Omniphony Studio release `vX.Y.Z` on `mgth/Omniphony`, from a
green `main` to a published GitHub release. Written against v0.5.0 and v0.5.1;
amend this file whenever a release teaches something new.

## Version tracks — one number does not fit all

| Component | Repo | Tag namespace | Version line |
|---|---|---|---|
| Omniphony Studio bundle | `mgth/Omniphony` | `v*` (e.g. `v0.5.1`) | stack version |
| Standalone liborender | `mgth/Omniphony` | `liborender-v*` | `orender_ffi` crate version (kept in step with the stack since 0.5.0) |
| mpv player bundle | assets on `mgth/Omniphony`, source in `mgth/mpv-omniphony` | `mpv-v*` | stack version |
| mpv fork branches | `mgth/mpv` | `orender-v*` | stack version (plain `v*` collides with upstream mpv's ancient tags) |
| harletty-bridge | `harletty/harletty-bridge` | `v*` | **its own line** (0.7.x) — never tag it with the stack number |

A patch release usually only involves the first track. Cut the others only when
their component actually changed.

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
  live; audio-path changes need that sign-off).
- Check whether the release also needs `liborender-v*` or `mpv-v*` (see the
  table above); those have their own steps below.

## 2. Version bump (PR to `main`)

Never push to `main` directly — open a PR. Six files change (verified at
0.5.0 `96bccc2` and 0.5.1):

1. `omniphony-studio/package.json` — `"version"`
2. `omniphony-studio/package-lock.json` — regenerate with
   `npm install --package-lock-only` (from `omniphony-studio/`)
3. `omniphony-studio/src-tauri/Cargo.toml` — `version`
4. `omniphony-studio/src-tauri/Cargo.lock` — regenerate with
   `cargo update -p omniphony-studio --offline` (from `src-tauri/`)
5. `omniphony-studio/src-tauri/tauri.conf.json` — `"version"`
6. `omniphony-renderer/orender_ffi/Cargo.toml` — `version` (in step with the
   stack since 0.5.0)

`omniphony-renderer/Cargo.lock` is gitignored — nothing to commit there.

Merge the PR once CI is green.

## 3. Promote `main` → `release`

Open a PR with **base=`release`, head=`main`** and merge it as a **merge
commit — not squash**. `release.yml`'s guard job checks
`git merge-base --is-ancestor <tag SHA> origin/release`; a squash rewrites the
SHAs and the guard rejects the tag.

`ci.yml` gates PRs to `release` too, so the promotion PR re-runs the full
suite it just ran on `main` — budget for two CI passes (~6 min each at 0.5.1)
between the bump merge and the tag.

Back-merge discipline: any hotfix committed directly on `release` must be
merged back into `main`, or `main` regresses at the next promotion.

## 4. Tag and build

```sh
git fetch origin release
git tag -a vX.Y.Z -m "Omniphony vX.Y.Z" origin/release
git push origin vX.Y.Z
```

The tag push triggers `release.yml`:

- **guard** — rejects tags whose SHA is not on `origin/release`.
- **build-studio** — Linux (`.deb`/`.rpm`/`.AppImage`), Windows
  (`.msi`/`.exe`), macOS arm64 (`.dmg`/`.app.tar.gz`, ad-hoc signed, not
  notarized). tauri-action creates a **draft** release named
  "Omniphony vX.Y.Z". Seven assets expected; whole run took ~17 min at 0.5.1
  (Linux is the slowest job at ~11 min).

The draft's URL is `releases/tag/untagged-<hash>` until it is published —
that is normal, not a broken tag association; it becomes `releases/tag/vX.Y.Z`
at publish.

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

```sh
gh release edit vX.Y.Z --repo mgth/Omniphony --notes-file notes.md
gh release edit vX.Y.Z --repo mgth/Omniphony --draft=false --latest
```

Only the Studio bundle `v*` release is marked `--latest`; `liborender-v*` and
`mpv-v*` are published not-latest.

To verify the latest marker, use `gh api repos/mgth/Omniphony/releases/latest`
— `gh release view --json` has no `isLatest` field.

## 6. Optional: standalone liborender release

Tag `liborender-v<orender_ffi crate version>` on `release`.
`liborender-release.yml` has **two** guards: the tag must be on `release`
**and** must equal the `orender_ffi` crate version. Builds a draft with the
standalone `.so`/`.dll`/`.dylib`. Skipped at 0.5.0 and 0.5.1.

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
| `mpv-omniphony` | when an `mpv-v*` bundle was cut: `_tag`, `depends=('orender>=X.Y.Z')` (the release-train couple) |
| `mpv-omniphony-fel` | with mpv-omniphony; also refresh `_mpvcommit` to the mpv master SHA the local FEL build verified (`scripts/build-fel-local.sh`) |
| `harletty-bridge` | on its own 0.7.x line only — never the stack number |

Per package: bump `pkgver` (+ `_tag`/pins), reset `pkgrel=1`, `updpkgsums`,
build-test with `makepkg -fCd` (`-d` because the runtime `orender` dep need
not be installed locally), `makepkg --printsrcinfo > .SRCINFO`, commit
`upgpkg: <pkg> X.Y.Z-1`, push `master`.

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
