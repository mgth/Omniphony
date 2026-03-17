@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
cd /d C:\dev
if exist openblas-build rmdir /s /q openblas-build
mkdir openblas-build
C:\dev\vcpkg\downloads\tools\cmake-3.31.10-windows\cmake-3.31.10-windows-x86_64\bin\cmake.exe -S C:\dev\vcpkg\buildtrees\openblas\src\v0.3.29-abfa9cf6a4.clean -B C:\dev\openblas-build -G "NMake Makefiles" -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_FLAGS_RELEASE="/MD /Od /DNDEBUG" -DBUILD_WITHOUT_LAPACK=OFF -DNOFORTRAN=ON -DC_LAPACK=ON -DBUILD_TESTING=OFF -DBUILD_SHARED_LIBS=OFF
if %ERRORLEVEL% neq 0 exit /b %ERRORLEVEL%
C:\dev\vcpkg\downloads\tools\cmake-3.31.10-windows\cmake-3.31.10-windows-x86_64\bin\cmake.exe --build C:\dev\openblas-build --config Release
