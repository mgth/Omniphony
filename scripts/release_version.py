#!/usr/bin/env python3
"""The release version, the one number every Omniphony component ships under.

`omniphony-renderer/Cargo.toml`'s `[workspace.package] version` is the source
of truth; Studio, orender and liborender all carry it. Its
copies live in manifests and lockfiles no tool keeps together, so this script
moves them as one and refuses a tree where they disagree (#676).

    release_version.py show                  print the release version
    release_version.py check [--tag TAG]     every copy agrees; with a tag, the
                                             tag names this version and the
                                             README's compatibility table opens
                                             with this release
    release_version.py set X.Y.Z [--player mpv-vX.Y.Z]
                                             move every copy, add the README row
    release_version.py manifest [--tag TAG]  the compatibility manifest (JSON)
                                             attached to the release

Three crates keep a line of their own and are never touched: `bridge_api`
(its version is the plugin ABI contract), `spdif` (the decoder bridges require
it by version from their repository) and the OSC contract.

Standard library only: it runs in the release guard before any toolchain is
installed.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RENDERER = ROOT / "omniphony-renderer"
STUDIO_EGUI = ROOT / "omniphony-studio-egui"
README = ROOT / "README.md"

# Renderer workspace members that keep their own version line.
OWN_LINE = {"bridge_api", "spdif"}

REPO_URL = "https://github.com/mgth/Omniphony"
TABLE_START = "<!-- compat-table:start -->"
TABLE_END = "<!-- compat-table:end -->"
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")
# `vX.Y.Z`, or a polish tag `vX.Y.Z.N` cut without a version bump.
TAG_RE = re.compile(r"^v(\d+\.\d+\.\d+)(?:\.\d+)?$")


class Mismatch(Exception):
    pass


def load_toml(path: Path) -> dict:
    with path.open("rb") as f:
        return tomllib.load(f)


def rel(path: Path) -> str:
    return str(path.relative_to(ROOT))


# ── Where the version lives ─────────────────────────────────────────────────


def release_version() -> str:
    return load_toml(RENDERER / "Cargo.toml")["workspace"]["package"]["version"]


def player_tag() -> str:
    return load_toml(RENDERER / "Cargo.toml")["workspace"]["metadata"]["release"]["player"]


def bridge_api_version() -> str:
    return load_toml(RENDERER / "bridge_api" / "Cargo.toml")["package"]["version"]


def liborender_abi() -> str:
    source = (RENDERER / "orender_ffi" / "src" / "lib.rs").read_text(encoding="utf-8")
    parts = []
    for name in ("ORENDER_ABI_MAJOR", "ORENDER_ABI_MINOR"):
        m = re.search(rf"^pub const {name}: u32 = (\d+);", source, re.M)
        if not m:
            raise Mismatch(f"orender_ffi/src/lib.rs: no `pub const {name}`")
        parts.append(m.group(1))
    return ".".join(parts)


def renderer_followers() -> list[str]:
    """Renderer packages on the release version: the root crate and every
    member outside `OWN_LINE`."""
    workspace = load_toml(RENDERER / "Cargo.toml")
    names = [workspace["package"]["name"]]
    for member in workspace["workspace"]["members"]:
        package = load_toml(RENDERER / member / "Cargo.toml")["package"]
        if package["name"] not in OWN_LINE:
            names.append(package["name"])
    return names


def egui_packages() -> list[str]:
    workspace = load_toml(STUDIO_EGUI / "Cargo.toml")
    return [
        load_toml(STUDIO_EGUI / member / "Cargo.toml")["package"]["name"]
        for member in workspace["workspace"]["members"]
    ]


def package_version(manifest: Path, workspace_version: str | None) -> str:
    """A package's version, resolving `version.workspace = true`."""
    version = load_toml(manifest)["package"]["version"]
    if isinstance(version, dict):
        if workspace_version is None:
            raise Mismatch(f"{rel(manifest)}: inherits a version from no workspace")
        return workspace_version
    return version


def copies() -> list[tuple[str, str]]:
    """Every place the release version is written, with what it says."""
    found: list[tuple[str, str]] = []
    renderer = load_toml(RENDERER / "Cargo.toml")
    workspace_version = renderer["workspace"]["package"]["version"]
    found.append(("omniphony-renderer/Cargo.toml [workspace.package]", workspace_version))
    found.append(("omniphony-renderer/Cargo.toml [package]",
                  package_version(RENDERER / "Cargo.toml", workspace_version)))
    for member in renderer["workspace"]["members"]:
        manifest = RENDERER / member / "Cargo.toml"
        if load_toml(manifest)["package"]["name"] in OWN_LINE:
            continue
        found.append((rel(manifest), package_version(manifest, workspace_version)))

    egui = load_toml(STUDIO_EGUI / "Cargo.toml")
    egui_version = egui["workspace"]["package"]["version"]
    found.append(("omniphony-studio-egui/Cargo.toml [workspace.package]", egui_version))
    for member in egui["workspace"]["members"]:
        manifest = STUDIO_EGUI / member / "Cargo.toml"
        found.append((rel(manifest), package_version(manifest, egui_version)))

    for lockfile, names in lockfiles():
        versions = {p["name"]: p["version"] for p in load_toml(lockfile)["package"]
                    if "source" not in p}
        for name in names:
            found.append((f"{rel(lockfile)} {name}", versions[name]))
    return found


def lockfiles() -> list[tuple[Path, list[str]]]:
    """Each lockfile with the release-version packages it records: whichever
    of the repository's own crates it reaches by path (the Studio, for one,
    builds omniphony_geometry from the renderer's tree). Read from the
    lockfile rather than listed, so a new path dependency is moved and
    checked without a change here."""
    own = set(renderer_followers()) | set(egui_packages())
    found = []
    for lockfile in (RENDERER / "Cargo.lock", STUDIO_EGUI / "Cargo.lock"):
        names = [p["name"] for p in load_toml(lockfile)["package"]
                 if "source" not in p and p["name"] in own]
        found.append((lockfile, names))
    return found


# ── The README's compatibility table ────────────────────────────────────────


def manifest(tag: str | None) -> dict:
    version = release_version()
    bridge = bridge_api_version().split(".")
    commit = None
    try:
        commit = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, check=True, capture_output=True, text=True
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        pass
    return {
        "release": tag or f"v{version}",
        "commit": commit,
        "studio": version,
        "orender": version,
        "liborender": {"version": version, "abi": liborender_abi()},
        "player": player_tag(),
        # A bridge loads in a host built against the same bridge_api minor.
        "bridge_api": f"{bridge[0]}.{bridge[1]}.x",
    }


def readme_row(m: dict) -> str:
    release, player = m["release"], m["player"]
    return (
        f"| [{release}]({REPO_URL}/releases/tag/{release}) "
        f"| {m['liborender']['abi']} "
        f"| [{player}]({REPO_URL}/releases/tag/{player}) "
        f"| {m['bridge_api']} |"
    )


def readme_table() -> tuple[str, list[str], str]:
    """The README split around the table's rows: text before, rows, text after."""
    text = README.read_text(encoding="utf-8")
    try:
        start = text.index(TABLE_START)
        end = text.index(TABLE_END)
    except ValueError:
        raise Mismatch(f"README.md: no {TABLE_START} … {TABLE_END} block") from None
    lines = text[start:end].splitlines()
    # The marker, the header and the separator, then the rows.
    head = "\n".join(lines[:3])
    rows = [line for line in lines[3:] if line.startswith("|")]
    return text[:start] + head + "\n", rows, "\n" + text[end:]


def row_release(row: str) -> str:
    m = re.match(r"\| \[(v[^\]]+)\]", row)
    return m.group(1) if m else ""


# ── Commands ────────────────────────────────────────────────────────────────


def check(tag: str | None) -> None:
    version = release_version()
    if not VERSION_RE.match(version):
        raise Mismatch(f"release version {version!r} is not X.Y.Z")
    wrong = [f"  {where}: {found}" for where, found in copies() if found != version]
    if wrong:
        raise Mismatch(
            f"these copies are not the release version {version}:\n" + "\n".join(wrong)
            + "\nmove them together with `scripts/release_version.py set X.Y.Z`"
        )
    if tag is None:
        return
    m = TAG_RE.match(tag)
    if not m or m.group(1) != version:
        raise Mismatch(
            f"tag {tag} does not name the release version {version} "
            f"(expected v{version} or v{version}.N)"
        )
    expected = readme_row(manifest(f"v{version}"))
    _, rows, _ = readme_table()
    if not rows or rows[0] != expected:
        raise Mismatch(
            "README.md's compatibility table does not open with this release:\n"
            f"  expected: {expected}\n  found:    {rows[0] if rows else '(no row)'}\n"
            "`scripts/release_version.py set` writes that row"
        )


def replace_version(path: Path, pattern: str, version: str, count: int = 1) -> None:
    """Rewrite the first `count` matches of `pattern` (whose group 1 precedes
    the version and group 2 follows it), keeping the file's formatting."""
    text = path.read_text(encoding="utf-8")
    new, n = re.subn(pattern, rf"\g<1>{version}\g<2>", text, count=count, flags=re.M)
    if n != count:
        raise Mismatch(f"{rel(path)}: expected {count} version field(s), found {n}")
    path.write_text(new, encoding="utf-8")


def set_toml_key(path: Path, section: str, key: str, value: str) -> None:
    """Rewrite one `key = "…"` line of one TOML section, keeping the rest."""
    text = path.read_text(encoding="utf-8")
    start = text.index(f"[{section}]\n")
    end = text.find("\n[", start + 1)
    end = len(text) if end < 0 else end
    body, n = re.subn(rf'^({re.escape(key)} = ")[^"]+(")', rf"\g<1>{value}\g<2>",
                      text[start:end], count=1, flags=re.M)
    if n != 1:
        raise Mismatch(f"{rel(path)}: no {key} in [{section}]")
    path.write_text(text[:start] + body + text[end:], encoding="utf-8")


def set_version(version: str, player: str | None) -> None:
    if not VERSION_RE.match(version):
        raise Mismatch(f"{version!r} is not X.Y.Z")
    if player is not None and not player.startswith("mpv-v"):
        raise Mismatch(f"player tag {player!r} is not an mpv-v* tag")

    set_toml_key(RENDERER / "Cargo.toml", "workspace.package", "version", version)
    set_toml_key(RENDERER / "omniphony_geometry" / "Cargo.toml", "package", "version", version)
    set_toml_key(STUDIO_EGUI / "Cargo.toml", "workspace.package", "version", version)
    if player is not None:
        set_toml_key(RENDERER / "Cargo.toml", "workspace.metadata.release", "player", player)
    for lockfile, names in lockfiles():
        for name in names:
            replace_version(
                lockfile,
                rf'^(\[\[package\]\]\nname = "{re.escape(name)}"\nversion = ")[^"]+(")',
                version,
            )

    before, rows, after = readme_table()
    row = readme_row(manifest(f"v{version}"))
    rows = [r for r in rows if row_release(r) != f"v{version}"]
    README.write_text(before + "\n".join([row] + rows) + after, encoding="utf-8")
    check(f"v{version}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("show")
    p = sub.add_parser("check")
    p.add_argument("--tag")
    p = sub.add_parser("set")
    p.add_argument("version")
    p.add_argument("--player", help="the mpv-v* tag this release ships with (default: unchanged)")
    p = sub.add_parser("manifest")
    p.add_argument("--tag")
    args = parser.parse_args()
    try:
        if args.command == "show":
            print(release_version())
        elif args.command == "check":
            check(args.tag)
            print(f"release version {release_version()}: every copy agrees")
        elif args.command == "set":
            set_version(args.version, args.player)
            print(f"release version {args.version}: every copy moved, README row written")
        elif args.command == "manifest":
            print(json.dumps(manifest(args.tag), indent=2))
    except Mismatch as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
