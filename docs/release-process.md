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
  `main`; no completed work is sitting uncommitted.
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

Source lives in `mgth/mpv-omniphony`, assets are published on
`mgth/Omniphony` under `mpv-v*`. Summary (see the memory fiche and
`docs/mpv-omniphony.md` for detail): bump `OMNIPHONY_REF` to the new tag in
its `release.yml`, regenerate `patches/` from the fork's `orender` branch,
copy `ad_orender.c`, PR to its `main`, tag `vX.Y.Z` there. Its PR CI builds
against a **mock liborender** — new FFI functions must be added to
`.github/scripts/mock-liborender.sh` or the tag build fails where the PR
passed. Pushing workflow-file changes needs the SSH remote
(`git@github.com-mgth:…`); the HTTPS token lacks the `workflow` scope.

## 8. Post-release checks

- macOS: verify the signed bundle still decodes (the 0.5.0 hardened-runtime
  regression, #260/#261) — check `codesign -d --entitlements` on the shipped
  app and confirm the bridge `dlopen` works on a real machine.
- First download on macOS: Gatekeeper behaviour (#201).
- Update the AUR packages if they track the release.
- Amend **this document** with anything the release taught.
