# Building omniphony-renderer with SAF-backed VBAP on Windows

This guide documents how to build `omniphony-renderer` with the `saf_vbap` feature on Windows,
enabling runtime VBAP gain table generation via the `generate-vbap` command.

Important naming note:

- the dependency actually used by `omniphony-renderer` is
  [`Spatial_Audio_Framework` (SAF)](https://github.com/leomccormack/Spatial_Audio_Framework)
- `SPARTA` is a separate plug-in suite built using SAF:
  https://leomccormack.github.io/sparta-site/
- the Cargo feature name `saf_vbap` specifically enables SAF-backed VBAP
  generation in `omniphony-renderer`

## Prerequisites

- **Visual Studio 2022** (Community or higher) with C++ desktop workload
- **Rust 1.87.0+** with the `x86_64-pc-windows-msvc` target
- **LLVM/Clang** installed (for `bindgen`) — download from https://github.com/llvm/llvm-project/releases
- **Git** (Git for Windows)

## Overview

The `saf_vbap` feature depends on two C libraries:
1. **SAF** (Spatial Audio Framework) — provides VBAP spatial audio algorithms
2. **OpenBLAS** (with LAPACK + LAPACKE) — linear algebra backend for SAF

Both must be built as **static libraries** with **MSVC**.

Licensing note:

- SAF upstream documents a dual-licensing model
- for `omniphony-renderer`, you should review the exact SAF configuration you build and the
  upstream terms that apply to it before redistributing binaries
- this repository does not bundle or redistribute SAF or SPARTA source/binaries

## Step 1: Clone and bootstrap vcpkg

```bash
git clone --depth 1 https://github.com/microsoft/vcpkg.git C:/dev/vcpkg
C:/dev/vcpkg/bootstrap-vcpkg.bat -disableMetrics
```

## Step 2: Build OpenBLAS from source (with LAPACKE)

> **Why from source?** vcpkg's `openblas` port builds with `-DBUILD_WITHOUT_LAPACK=ON`, which excludes LAPACKE. SAF requires the LAPACKE C interface functions (`LAPACKE_*_work`).

First, get the OpenBLAS source via vcpkg (which downloads and patches it):

```bash
C:/dev/vcpkg/vcpkg.exe install openblas:x64-windows
```

Then rebuild from that source with LAPACK enabled. Create `C:\dev\build_openblas.bat`:

```bat
@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
cd /d C:\dev
if exist openblas-build rmdir /s /q openblas-build
mkdir openblas-build

C:\dev\vcpkg\downloads\tools\cmake-3.31.10-windows\cmake-3.31.10-windows-x86_64\bin\cmake.exe ^
  -S C:\dev\vcpkg\buildtrees\openblas\src\v0.3.29-abfa9cf6a4.clean ^
  -B C:\dev\openblas-build ^
  -G "NMake Makefiles" ^
  -DCMAKE_BUILD_TYPE=Release ^
  -DCMAKE_C_FLAGS_RELEASE="/MD /Od /DNDEBUG" ^
  -DBUILD_WITHOUT_LAPACK=OFF ^
  -DNOFORTRAN=ON ^
  -DC_LAPACK=ON ^
  -DBUILD_TESTING=OFF ^
  -DBUILD_SHARED_LIBS=OFF
if %ERRORLEVEL% neq 0 exit /b %ERRORLEVEL%

C:\dev\vcpkg\downloads\tools\cmake-3.31.10-windows\cmake-3.31.10-windows-x86_64\bin\cmake.exe ^
  --build C:\dev\openblas-build --config Release
```

Run it: `cmd /c C:\dev\build_openblas.bat`

Output: `C:\dev\openblas-build\lib\Release\openblas.lib` (~55 MB static lib with BLAS + LAPACK + LAPACKE)

Then install it to vcpkg's directory:

```bash
# Replace vcpkg's openblas.lib with the full build
cp C:/dev/openblas-build/lib/Release/openblas.lib C:/dev/vcpkg/installed/x64-windows/lib/openblas.lib

# Copy LAPACKE headers (needed by SAF at compile time and by bindgen)
cp C:/dev/vcpkg/buildtrees/openblas/src/v0.3.29-*/lapack-netlib/LAPACKE/include/lapacke*.h \
   C:/dev/vcpkg/installed/x64-windows/include/
cp C:/dev/vcpkg/buildtrees/openblas/src/v0.3.29-*/lapack-netlib/LAPACKE/include/lapack.h \
   C:/dev/vcpkg/installed/x64-windows/include/
```

## Step 3: Build SAF

Copy SAF source to a local drive (MSVC cannot build on network shares):

```bash
mkdir -p C:/dev/SAF
cp -r SPARTA/SDKs/Spatial_Audio_Framework/* C:/dev/SAF/
rm -rf C:/dev/SAF/build C:/dev/SAF/build-win  # remove any stale builds
```

Create `C:\dev\build_saf.bat`:

```bat
@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
cd /d C:\dev\SAF
if exist build-win rmdir /s /q build-win

C:\dev\vcpkg\downloads\tools\cmake-3.31.10-windows\cmake-3.31.10-windows-x86_64\bin\cmake.exe ^
  -S . -B build-win ^
  -G "NMake Makefiles" ^
  -DCMAKE_BUILD_TYPE=Release ^
  -DCMAKE_C_FLAGS_RELEASE="/MD /O1 /DNDEBUG /DWIN32" ^
  -DSAF_PERFORMANCE_LIB=SAF_USE_OPEN_BLAS_AND_LAPACKE ^
  -DSAF_BUILD_EXAMPLES=OFF ^
  -DSAF_BUILD_TESTS=OFF ^
  -DBUILD_SHARED_LIBS=OFF ^
  -DCMAKE_TOOLCHAIN_FILE=C:\dev\vcpkg\scripts\buildsystems\vcpkg.cmake ^
  -DOPENBLAS_LIBRARY=C:\dev\vcpkg\installed\x64-windows\lib\openblas.lib ^
  -DLAPACKE_LIBRARY=C:\dev\vcpkg\installed\x64-windows\lib\openblas.lib ^
  -DOPENBLAS_HEADER_PATH=C:\dev\vcpkg\installed\x64-windows\include\openblas ^
  -DLAPACKE_HEADER_PATH=C:\dev\vcpkg\installed\x64-windows\include
if %ERRORLEVEL% neq 0 exit /b %ERRORLEVEL%

C:\dev\vcpkg\downloads\tools\cmake-3.31.10-windows\cmake-3.31.10-windows-x86_64\bin\cmake.exe ^
  --build build-win --config Release
```

Run it: `cmd /c C:\dev\build_saf.bat`

Output: `C:\dev\SAF\build-win\framework\saf.lib` (~3.3 MB)

> **Note:** We use `/O1` (optimize for size) because:
> 1. MSVC 19.44 has an internal compiler error (ICE) in `saf_utility_filters.c` at `/O2` (only that one file is affected)
> 2. `/O1` is actually **faster** than `/O2` for this workload (~2.4s vs ~3.1s for VBAP generation), likely due to better instruction cache utilization with smaller code
> 3. `/O1` is 21% faster than `/Od` (no optimization)
>
> `clang-cl` is not an alternative due to C99 `_Complex` vs MSVC `_Fcomplex` type mismatches. This may be fixed in a future MSVC update.

## Step 4: Build omniphony-renderer

Set environment variables and build:

```bash
export LIBCLANG_PATH="C:/Program Files/LLVM/lib"
export VCPKG_ROOT="C:/dev/vcpkg"
export SAF_ROOT="C:/dev/SAF"
export CPAL_ASIO_DIR="C:/dev/asio_sdk"

# SAF-backed VBAP + ASIO + Windows Service (full Windows build)
cargo build --release --features saf_vbap,asio

# SAF-backed VBAP only (no ASIO, no Windows Service)
cargo build --release --features saf_vbap
```

### Environment variables reference

| Variable | Purpose | Default |
|---|---|---|
| `SAF_ROOT` | Path to SAF source tree | `../SPARTA/SDKs/Spatial_Audio_Framework` |
| `VCPKG_ROOT` | Path to vcpkg installation | (none, required) |
| `OPENBLAS_PATH` | Direct path to OpenBLAS lib dir (bypasses vcpkg) | (none, optional) |
| `LIBCLANG_PATH` | Path to LLVM/Clang `lib/` directory (for bindgen) | (none, required if not on PATH) |
| `CPAL_ASIO_DIR` | Path to Steinberg ASIO SDK directory | (none, **required** — place the SDK at `C:/dev/asio_sdk`) |

### Feature combinations

| Features | Commands available |
|---|---|
| `saf_vbap` | default render flow, `generate-vbap` |
| `asio` | default render flow, `list-asio-devices` (`--output-backend asio`) |
| `saf_vbap,asio` | All of the above |

### Building from a different directory

If building from a directory other than the repo root (e.g., `C:\dev\Omniphony\omniphony-renderer`), use `SAF_ROOT` to point to the SAF source:

```bash
export SAF_ROOT="C:/dev/SAF"
cargo build --features saf_vbap,asio --release
```

## Step 5: Verify

```bash
orender.exe --help
# Should show "generate-vbap" and "list-asio-devices" in the commands list

orender.exe generate-vbap --speaker-layout ..\\layouts\\7.1.4.yaml --output test.vbap
# Should generate a VBAP gain table file
```

## Optimization benchmarks

VBAP generation performance for a 7.1.4 layout (12 speakers, 22 triangles, 5 spread tables at 1° resolution):

| MSVC flag | Description | Time | vs `/O1` |
|---|---|---|---|
| `/O1` | Optimize for size | **2.41s** | — |
| `/O2` | Optimize for speed | 3.08s | +28% slower |
| `/Od` | No optimization | 3.07s | +27% slower |

`/O1` produces the fastest code for this workload. The smaller code likely fits better in the CPU instruction cache, outperforming `/O2`'s larger "speed-optimized" output.

The `/O2` benchmark was done by setting `/O2` per-file on all source files except `saf_utility_filters.c` (which causes an ICE at `/O2` and was kept at `/O1`). Results were consistent across 3 runs.

## Troubleshooting

### MSVC Internal Compiler Error (C1001) in saf_utility_filters.c
Use `/O1` in `CMAKE_C_FLAGS_RELEASE`. This is a known MSVC 19.44 bug in the `/O2` optimizer pass (`p2/main.cpp:258`). Only `saf_utility_filters.c` is affected — all other SAF files compile fine with `/O2`. Using `/O1` globally is the simplest workaround.

### `LAPACKE_*_work` unresolved symbols at link time
Your `openblas.lib` was built without LAPACK. Rebuild OpenBLAS from source with `-DBUILD_WITHOUT_LAPACK=OFF -DC_LAPACK=ON` (see Step 2).

### `error creating or communicating with child process` (D8040)
MSVC cannot build on network drives. Copy source to a local drive (C:\).

### bindgen fails to find `clang`
Set `LIBCLANG_PATH` to point to your LLVM installation's `lib/` directory.

### SAF library not found
Set `SAF_ROOT` to the directory containing `framework/include/saf.h`. The build script looks for `saf.lib` in `build-win/framework/` and `build/framework/Release/`.

## File layout after setup

```
C:\dev\
  asio_sdk\                        # Steinberg ASIO SDK (set CPAL_ASIO_DIR to this)
    common\asio.h
    host\pc\asiolist.h
  vcpkg\                          # vcpkg with custom openblas.lib
    installed\x64-windows\
      lib\openblas.lib             # Full OpenBLAS with LAPACKE (55 MB)
      include\
        openblas\cblas.h
        lapacke.h                  # LAPACKE C interface headers
  SAF\                             # SAF source (copied from SPARTA/SDKs/...)
    build-win\framework\saf.lib    # SAF static library (3.3 MB)
    framework\include\saf.h
  openblas-build\                  # OpenBLAS build tree (can be deleted after install)
```
