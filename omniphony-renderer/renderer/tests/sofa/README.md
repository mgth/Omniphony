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

The files are copied byte for byte from those repositories; none was edited
here. libmysofa's three larger `fail-issue-{72,77,79}.sofa` (3.3 MB each,
derived from the CIPIC database) are left out.
