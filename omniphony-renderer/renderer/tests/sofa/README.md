# SOFA test files

Small SOFA files for `tests/sofa_files.rs`, which runs both readers
(`hrir_set_from_sofa`, `BrirSet::from_sofa`) on them. The files are test data
only and are never shipped.

| File | What it is | Source | Licence |
|------|------------|--------|---------|
| `Pulse.sofa` | SimpleFreeFieldHRIR, 1250 directions, written by the SOFA Matlab API | Piotr Majdak, Acoustics Research Institute, Vienna, via [sofacoustics.org `sofa_api_mo_test`](http://sofacoustics.org/data/sofa_api_mo_test/) and [libmysofa](https://github.com/hoene/libmysofa) `tests/` | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| `tester.sofa`, `tester2.sofa` | `Pulse.sofa` modified and saved again by libmysofa's `tests/tester.m` and `tester2.m` | Christian Hoene, libmysofa `tests/` | CC BY 4.0 (derived from `Pulse.sofa`) |
| `malformed/fail-issue-*.sofa` | Fuzzed copies of `tester.sofa`/`tester2.sofa` that crashed libmysofa, each named after its libmysofa issue | libmysofa `tests/` | CC BY 4.0 (derived from `Pulse.sofa`) |
| `sofasonix_netcdf4.sofa` | SimpleFreeFieldHRIR written through netCDF4 by SOFAsonix: its object headers are the case of issue #185 | [mgth/sofar](https://github.com/mgth/sofar) `tests/data/` | MIT OR Apache-2.0 |
| `chunked_multispeaker_brir.sofa` | MultiSpeakerBRIR, three speakers, chunked datasets | mgth/sofar `tests/data/` | MIT OR Apache-2.0 |
| `room_corners_cartesian.sofa` | Shoebox corners in cartesian metres; encoding on the `RoomCorners` variable | Derived here from `chunked_multispeaker_brir.sofa` by `gen_room_corners.py` | MIT OR Apache-2.0 |
| `room_corners_spherical.sofa` | The same corners in spherical degrees/degrees/metres; encoding on `RoomCorners` | Same generator and base | MIT OR Apache-2.0 |
| `room_corners_own_metadata.sofa` | Spherical corners with `Type`/`Units` only on each corner variable | Same generator and base | MIT OR Apache-2.0 |
| `room_corners_unsupported_unit.sofa` | Cartesian corners in feet: the box is refused, the BRIR still loads | Same generator and base | MIT OR Apache-2.0 |
| `room_corners_offset_listener.sofa` | Cartesian corners with the listener at `(3, 2, 1.2)` metres | Same generator and base | MIT OR Apache-2.0 |

Except for the generated `room_corners_*.sofa` variants, the files are copied
byte for byte from those repositories. libmysofa's three larger
`fail-issue-{72,77,79}.sofa` (3.3 MB each, derived from the CIPIC database)
are left out.

## Regenerating the room fixtures

Run from the repository root:

```sh
python3 omniphony-renderer/renderer/tests/sofa/gen_room_corners.py
cargo test --manifest-path omniphony-renderer/Cargo.toml --locked -p renderer --features sofa --test sofa_files
```

The generator uses Python's standard library via `ctypes` and the system's
shared `libhdf5` / `libhdf5_hl` (also found under Debian's `libhdf5_serial`
names). These libraries are only needed to regenerate the files, not to run
the Rust tests. New datasets have timestamps disabled; regeneration with
the same HDF5 version produces identical bytes.

Each variant sets the global `RoomType` to `shoebox` and adds `[I][C]` double
datasets `RoomCornerA` and `RoomCornerB`. The valid corners are `(0, 0, 0)`
and `(6, 4, 2.5)` in SOFA cartesian metres. The base listener is at the
origin, giving renderer corners `(0, 0, 0)` and `(-4, 6, 2.5)`; the offset
listener gives `(2, -3, -1.2)` and `(-2, 3, 1.3)`. The spherical values are
computed from those same cartesian corners. The unsupported-unit variant
keeps the cartesian numbers but labels them `foot`.

The shared `RoomCorners` variable contains an unused zero and carries
`Type` / `Units` attributes alone. The own-metadata variant omits it and
puts those attributes on each corner instead. No global coordinate metadata
is added. All original datasets are preserved except `ListenerPosition` in
the offset-listener variant; the impulse responses are unchanged in all
five files.
