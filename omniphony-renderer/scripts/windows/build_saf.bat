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
