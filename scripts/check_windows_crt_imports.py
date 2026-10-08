#!/usr/bin/env python3
"""Fail when a Windows binary depends on the Visual C++ runtime DLLs.

The Windows builds of `orender.exe`, `orender.dll` and the Studio executable
link the C runtime statically (`.cargo/config.toml` in each workspace,
docs/installer.md step 2), so they start on a Windows without the Visual C++
Redistributable. A build that loses the flag still links and runs on any
machine that has the redistributable, CI runners included; only its import
table shows it. This reads the import tables (normal and delay-load) of each
PE file given and exits non-zero if one names a runtime DLL:

    check_windows_crt_imports.py FILE.exe FILE.dll ...

Standard library only, no Visual Studio environment needed, so it runs the
same on the Windows runners and on Linux against a downloaded artifact.
"""

from __future__ import annotations

import fnmatch
import struct
import sys

# The DLLs the Redistributable installs, and the UCRT forwarders the dynamic
# CRT imports through. UCRT itself ships with Windows 10 and later, but a
# binary that imports api-ms-win-crt-* is one built against the dynamic CRT,
# and VCRUNTIME140.dll comes with it, so those are refused too.
FORBIDDEN = (
    "vcruntime*.dll",
    "msvcp*.dll",
    "concrt*.dll",
    "vccorlib*.dll",
    "msvcr1*.dll",
    "ucrtbase*.dll",
    "api-ms-win-crt-*.dll",
)

IMPORT_DIR = 1
DELAY_IMPORT_DIR = 13


class PeError(Exception):
    pass


def _u16(data: bytes, off: int) -> int:
    return struct.unpack_from("<H", data, off)[0]


def _u32(data: bytes, off: int) -> int:
    return struct.unpack_from("<I", data, off)[0]


def _cstr(data: bytes, off: int) -> str:
    end = data.find(b"\0", off)
    if end < 0:
        raise PeError(f"unterminated name at offset {off:#x}")
    return data[off:end].decode("ascii", "replace")


def imported_dlls(data: bytes) -> list[str]:
    """Names of the DLLs a PE image imports, normal and delay-load, in order."""
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise PeError("not a PE file (no MZ header)")
    pe = _u32(data, 0x3C)
    if data[pe : pe + 4] != b"PE\0\0":
        raise PeError("not a PE file (no PE signature)")
    coff = pe + 4
    nsections = _u16(data, coff + 2)
    opt_size = _u16(data, coff + 16)
    opt = coff + 20
    magic = _u16(data, opt)
    if magic == 0x10B:  # PE32
        dirs = opt + 96
    elif magic == 0x20B:  # PE32+
        dirs = opt + 112
    else:
        raise PeError(f"unknown optional header magic {magic:#x}")
    ndirs = _u32(data, dirs - 4)

    sections = []
    for i in range(nsections):
        s = opt + opt_size + 40 * i
        vsize, vaddr, rawsize, rawptr = struct.unpack_from("<IIII", data, s + 8)
        sections.append((vaddr, max(vsize, rawsize), rawptr))

    def rva_to_off(rva: int) -> int:
        for vaddr, size, rawptr in sections:
            if vaddr <= rva < vaddr + size:
                return rva - vaddr + rawptr
        raise PeError(f"RVA {rva:#x} is in no section")

    def directory(index: int) -> tuple[int, int]:
        if index >= ndirs:
            return 0, 0
        return struct.unpack_from("<II", data, dirs + 8 * index)

    names = []
    rva, size = directory(IMPORT_DIR)
    if rva and size:
        # IMAGE_IMPORT_DESCRIPTOR: 20 bytes, Name RVA at +12, ends with zeros.
        off = rva_to_off(rva)
        while True:
            desc = data[off : off + 20]
            if len(desc) < 20 or desc == b"\0" * 20:
                break
            names.append(_cstr(data, rva_to_off(_u32(desc, 12))))
            off += 20
    rva, size = directory(DELAY_IMPORT_DIR)
    if rva and size:
        # IMAGE_DELAYLOAD_DESCRIPTOR: 32 bytes, Name RVA at +4, ends with zeros.
        off = rva_to_off(rva)
        while True:
            desc = data[off : off + 32]
            if len(desc) < 32 or desc == b"\0" * 32:
                break
            name_rva = _u32(desc, 4)
            if not _u32(desc, 0) & 1:
                # Pre-VC7 layout: a virtual address, not an RVA.
                raise PeError("delay-load descriptor without the RVA attribute")
            names.append(_cstr(data, rva_to_off(name_rva)))
            off += 32
    return names


def forbidden(names: list[str]) -> list[str]:
    return [n for n in names if any(fnmatch.fnmatch(n.lower(), p) for p in FORBIDDEN)]


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    failed = False
    for path in argv:
        try:
            with open(path, "rb") as f:
                names = imported_dlls(f.read())
        except (OSError, PeError, struct.error) as e:
            print(f"{path}: {e}", file=sys.stderr)
            failed = True
            continue
        bad = forbidden(names)
        print(f"{path}: imports {', '.join(names) or 'nothing'}")
        if bad:
            print(
                f"{path}: depends on the Visual C++ runtime ({', '.join(bad)}): "
                "built without -C target-feature=+crt-static. A RUSTFLAGS or "
                "CARGO_ENCODED_RUSTFLAGS variable replaces the workspace's "
                ".cargo/config.toml, and cargo only reads that file when it "
                "runs from inside the workspace.",
                file=sys.stderr,
            )
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
