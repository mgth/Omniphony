# Install a composed installer (build.sh) of real release files for the
# current user, silently, and check what the install page checks: the engine
# in the per-user folder, the player loading it from there, orender starting
# without a Visual C++ runtime, and an uninstall that keeps the config. Run
# by installer.yml on windows-latest.
#
#   pwsh packaging/installer/windows/verify-release.ps1 -Setup <setup.exe>
param([Parameter(Mandatory)][string]$Setup)
$ErrorActionPreference = 'Stop'
function Fail($message) { throw "FAIL: $message" }

$dir = Join-Path $env:LOCALAPPDATA 'Programs\Omniphony'
$lib = Join-Path $env:LOCALAPPDATA 'omniphony\lib'
$config = Join-Path $env:ProgramData 'omniphony'

$code = (Start-Process -FilePath $Setup -ArgumentList '/S' -Wait -PassThru).ExitCode
if ($code -ne 0) { Fail "the installer returned $code" }
if (-not (Test-Path "$lib\orender.dll")) { Fail "no engine in $lib" }

Write-Host '== orender starts'
& "$dir\studio\orender.exe" --version
if ($LASTEXITCODE -ne 0) { Fail "orender --version returned $LASTEXITCODE" }

Write-Host '== the player loads the engine from the per-user folder'
# One second of 5.1 tone, encoded to E-AC-3 by the player's own FFmpeg: a
# codec ad_orender takes, so it loads the engine (no bridge here, the stream
# then falls back to mpv's decoder).
$work = Join-Path $env:TEMP 'omniphony-verify'
New-Item -ItemType Directory -Force $work | Out-Null
$wav = Join-Path $work 'tone51.wav'
python -c @"
import math, struct, sys, wave
with wave.open(sys.argv[1], 'wb') as w:
    w.setnchannels(6); w.setsampwidth(2); w.setframerate(48000)
    w.writeframes(b''.join(
        struct.pack('<h', int(8000 * math.sin(2 * math.pi * 440 * i / 48000))) * 6
        for i in range(48000)))
"@ $wav
$mpv = "$dir\player\mpv.com"
$probe = Join-Path $work 'probe.mka'
& $mpv --no-config --really-quiet $wav "--o=$probe" --oac=eac3 --of=matroska
if (-not (Test-Path $probe)) { Fail 'the player could not encode the probe' }
$log = & $mpv --no-config '--ad=orender,lavc' --ao=null --vo=null $probe 2>&1 | Out-String
$expected = [regex]::Escape("$lib\orender.dll") + ' \(studio install\)'
$line = $log -split "`n" | Where-Object { $_ -match "orender: loaded liborender .*$expected" } | Select-Object -First 1
if (-not $line) { Write-Host $log; Fail 'the player did not load the installed engine' }
Write-Host $line.Trim()

Write-Host '== uninstall keeps the config'
New-Item -ItemType Directory -Force $config | Out-Null
Set-Content "$config\config.yaml" 'render: {}'
$code = (Start-Process -FilePath "$dir\uninstall.exe" -ArgumentList '/S' -Wait -PassThru).ExitCode
if ($code -ne 0) { Fail "uninstall.exe returned $code" }
foreach ($i in 1..60) {
    if (-not (Test-Path "$dir\uninstall.exe")) { break }
    Start-Sleep -Milliseconds 500
}
if (Test-Path "$dir\player\mpv.exe") { Fail 'uninstall left the player' }
if (Test-Path "$lib\orender.dll") { Fail 'uninstall left the engine' }
if (-not (Test-Path "$config\config.yaml")) { Fail 'uninstall removed config.yaml' }
Write-Host '>> installer OK'
