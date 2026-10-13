# The Windows installer end to end on stand-in release files, on a real
# Windows: per user (install, upgrade with the logon start, a file in use,
# uninstall) and per machine with the renderer service. Run by installer.yml
# after make-standins.sh, from an elevated session (the per-machine half
# creates a service):
#
#   pwsh packaging/installer/windows/test.ps1 -Out <dir of make-standins.sh>
param([Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Stop'

$v1 = Join-Path $Out 'omniphony-v1.0-windows-x86_64-setup.exe'
$v2 = Join-Path $Out 'omniphony-v2.0-windows-x86_64-setup.exe'
$uninstKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\Omniphony'
$renderArgs = 'render \\.\pipe\orender.input --continuous --enable-vbap --osc --osc-host 127.0.0.1 --osc-port 9000 --osc-rx-port 9000 --osc-yield'

function Fail($message) { throw "FAIL: $message" }
function Has($path) { if (-not (Test-Path -LiteralPath $path)) { Fail "missing $path" } }
function HasNot($path) { if (Test-Path -LiteralPath $path) { Fail "still there: $path" } }
function Setup($exe, [string[]]$switches) {
    (Start-Process -FilePath $exe -ArgumentList $switches -Wait -PassThru).ExitCode
}
# The uninstaller hands over to a copy of itself and returns at once: wait
# for that copy to have removed the uninstaller.
function Uninstall($dir) {
    $code = Setup (Join-Path $dir 'uninstall.exe') @('/S')
    if ($code -ne 0) { Fail "uninstall.exe returned $code" }
    foreach ($i in 1..60) {
        if (-not (Test-Path -LiteralPath (Join-Path $dir 'uninstall.exe'))) { return }
        Start-Sleep -Milliseconds 500
    }
    Fail "the uninstaller did not finish in $dir"
}
function UninstallValue($hive, $name) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, [Microsoft.Win32.RegistryView]::Registry64)
    $key = $base.OpenSubKey($uninstKey)
    if ($null -eq $key) { return $null }
    $key.GetValue($name)
}

$dir = Join-Path $env:LOCALAPPDATA 'Programs\Omniphony'
$lib = Join-Path $env:LOCALAPPDATA 'omniphony\lib'
$config = Join-Path $env:ProgramData 'omniphony'
$menu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
$startup = Join-Path $menu 'Startup\Omniphony Renderer.lnk'

Write-Host '== per user: install 1.0'
if ((Setup $v1 @('/S')) -ne 0) { Fail 'install 1.0' }
Has "$dir\player\mpv.exe"; Has "$dir\studio\orender.exe"; Has "$dir\studio\only-in-1.0.txt"
Has "$dir\studio\layouts\legacy\5.1.yaml"; Has "$dir\uninstall.exe"
if ((Get-Content "$lib\orender.dll") -ne 'engine-v1.0') { Fail 'engine not in the per-user folder' }
if ((Get-Content "$dir\player\orender.dll") -ne 'fallback') { Fail "the player's own engine was replaced in a per-user install" }
Has $config
HasNot "$config\config.yaml"
Has "$menu\Omniphony\Omniphony Studio.lnk"; Has "$menu\Omniphony\mpv-omniphony.lnk"
HasNot $startup
if ((UninstallValue CurrentUser 'DisplayVersion') -ne 'v1.0') { Fail 'uninstall entry' }

Write-Host '== a bridge next to the engine and next to orender, a config'
Set-Content "$lib\test_bridge.dll" 'bridge'
Set-Content "$dir\studio\test_bridge.dll" 'bridge'
Set-Content "$config\config.yaml" 'render: {}'

Write-Host '== a file in use refuses the upgrade'
$held = [System.IO.File]::Open("$dir\studio\orender.exe", 'Open', 'Read', 'Read')
try {
    $code = Setup $v2 @('/S')
    if ($code -eq 0) { Fail 'the installer wrote over a file in use' }
} finally { $held.Close() }
Has "$dir\studio\only-in-1.0.txt"

Write-Host '== upgrade to 2.0 with the logon start'
if ((Setup $v2 @('/S', '/STARTUP')) -ne 0) { Fail 'install 2.0' }
if ((Get-Content "$lib\orender.dll") -ne 'engine-v2.0') { Fail 'engine not upgraded' }
HasNot "$dir\studio\only-in-1.0.txt"
Has "$dir\studio\test_bridge.dll"; Has "$lib\test_bridge.dll"
Has $startup
$link = (New-Object -ComObject WScript.Shell).CreateShortcut($startup)
if ($link.TargetPath -ne "$dir\studio\orender.exe") { Fail "logon start target: $($link.TargetPath)" }
if ($link.Arguments -ne $renderArgs) { Fail "logon start arguments: $($link.Arguments)" }
if ((UninstallValue CurrentUser 'DisplayVersion') -ne 'v2.0') { Fail 'uninstall entry not upgraded' }

Write-Host '== an upgrade without the switch keeps the logon start'
if ((Setup $v2 @('/S')) -ne 0) { Fail 'second install of 2.0' }
Has $startup

Write-Host '== per user: uninstall'
Uninstall $dir
HasNot "$dir\player"; HasNot "$dir\studio\orender.exe"; HasNot "$lib\orender.dll"
HasNot $startup; HasNot "$menu\Omniphony"
Has "$dir\studio\test_bridge.dll"; Has "$lib\test_bridge.dll"; Has "$config\config.yaml"
if ($null -ne (UninstallValue CurrentUser 'DisplayVersion')) { Fail 'uninstall entry left' }
Remove-Item -Recurse -Force $dir, $lib

Write-Host '== /SERVICE needs /ALLUSERS'
if ((Setup $v2 @('/S', '/SERVICE')) -eq 0) { Fail '/SERVICE was accepted for a per-user install' }
HasNot $dir

Write-Host '== per machine, with the renderer service'
$machine = Join-Path $env:ProgramFiles 'Omniphony'
if ((Setup $v2 @('/S', '/ALLUSERS', '/SERVICE')) -ne 0) { Fail 'per-machine install' }
Has "$machine\studio\orender.exe"
if ((Get-Content "$machine\player\orender.dll") -ne 'engine-v2.0') { Fail 'engine not next to the player' }
if ((UninstallValue LocalMachine 'InstallLocation') -ne $machine) { Fail 'per-machine uninstall entry' }
$service = Get-CimInstance Win32_Service -Filter "Name='omniphony-renderer'"
if ($null -eq $service) { Fail 'no renderer service' }
$expected = "`"$machine\studio\orender.exe`" $renderArgs"
if ($service.PathName -ne $expected) { Fail "service command line: $($service.PathName)" }
if ($service.StartMode -ne 'Manual') { Fail "service start mode: $($service.StartMode)" }
if ($service.DisplayName -ne 'Omniphony Renderer') { Fail "service display name: $($service.DisplayName)" }

Write-Host '== installed again with /SERVICE=auto: the same service, reconfigured'
if ((Setup $v2 @('/S', '/ALLUSERS', '/SERVICE=auto')) -ne 0) { Fail 'per-machine reinstall' }
$service = Get-CimInstance Win32_Service -Filter "Name='omniphony-renderer'"
if ($service.StartMode -ne 'Auto') { Fail "service start mode after =auto: $($service.StartMode)" }
if ($service.PathName -ne $expected) { Fail "service command line after =auto: $($service.PathName)" }

Write-Host '== per machine: uninstall'
Uninstall $machine
HasNot $machine
if ($null -ne (Get-CimInstance Win32_Service -Filter "Name='omniphony-renderer'")) { Fail 'the service is still there' }
if ($null -ne (UninstallValue LocalMachine 'InstallLocation')) { Fail 'per-machine uninstall entry left' }
Has "$config\config.yaml"

Write-Host 'OK'
