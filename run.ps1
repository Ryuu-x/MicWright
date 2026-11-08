# Builds (if needed) and launches MicWright.
#   .\run.ps1                 build + run the WinUI 3 app (default)
#   .\run.ps1 -Settings 3     open straight on a sound-settings page (0 Cleanup, 1 Level, 2 Tone, 3 Effects)
#   .\run.ps1 -Gpui           run the older GPUI version instead
#   .\run.ps1 -SelfTest       headless audio check (no window, nothing played)
param([switch]$Gpui, [switch]$SelfTest, [int]$Settings = -1)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$env:Path += ";$env:USERPROFILE\.cargo\bin"

if ($Gpui -or $SelfTest) {
    cargo build --release -p mw-app
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $exe = Join-Path $PSScriptRoot 'target\release\micwright.exe'
    if ($SelfTest) {
        $out = Join-Path $env:TEMP 'mw-selftest.txt'
        $p = Start-Process $exe -ArgumentList '--selftest' -RedirectStandardOutput $out -Wait -PassThru -NoNewWindow
        Get-Content $out
        exit $p.ExitCode
    }
} else {
    # Rust engine DLL first, then the WinUI 3 front-end that loads it.
    cargo build --release -p mw-engine
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    dotnet build ui-winui -c Release --nologo -v quiet
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $exe = Join-Path $PSScriptRoot 'ui-winui\bin\Release\net10.0-windows10.0.26100.0\win-x64\MicWright.exe'
}

$argList = if ($Settings -ge 0) { @('--settings', "$Settings") } else { @() }
if ($argList) { Start-Process $exe -ArgumentList $argList } else { Start-Process $exe }
