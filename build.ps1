# Builds a self-contained release of MicWright.
#   .\build.ps1            -> dist\MicWright-<version>-Setup.exe   (installer, needs Inno Setup 6)
#                             dist\MicWright-<version>-win-x64.zip  (portable)
# The target PC needs nothing installed (.NET + Windows App SDK are bundled).
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$env:Path += ";$env:USERPROFILE\.cargo\bin"

$version = ([xml](Get-Content ui-winui\MicWright.WinUI.csproj)).Project.PropertyGroup.Version | Where-Object { $_ } | Select-Object -First 1
$name = "MicWright-$version-win-x64"
$stage = Join-Path $PSScriptRoot "dist\$name"
$zip = Join-Path $PSScriptRoot "dist\$name.zip"

Write-Host "== Tests"
cargo test --release --workspace --quiet
if ($LASTEXITCODE -ne 0) { throw "tests failed" }

Write-Host "== Third-party notices"
cargo run --release -p third-party-notices
if ($LASTEXITCODE -ne 0) { throw "notices failed" }

Write-Host "== Rust engine (mw_engine.dll)"
cargo build --release -p mw-engine
if ($LASTEXITCODE -ne 0) { throw "engine build failed" }

Write-Host "== WinUI 3 app (self-contained publish)"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
dotnet publish ui-winui -c Release -r win-x64 --self-contained true -p:PublishReadyToRun=true -o $stage --nologo -v quiet
if ($LASTEXITCODE -ne 0) { throw "publish failed" }

Copy-Item packaging\README.txt, LICENSE, THIRD-PARTY-NOTICES.txt $stage
# Debug symbols aren't needed by users.
Get-ChildItem $stage -Filter *.pdb | Remove-Item

Write-Host "== Smoke test"
$exe = Join-Path $stage 'MicWright.exe'
# --background: no window. If a copy is already running, this one hands over and exits with 0.
$p = Start-Process $exe -ArgumentList '--background' -PassThru
Start-Sleep 6
$p.Refresh()
if ($p.HasExited -and $p.ExitCode -ne 0) { throw "the published app exited immediately (code $($p.ExitCode))" }
if (-not $p.HasExited) { Stop-Process -Id $p.Id }

Write-Host "== Zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal
$mb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host "Done: $zip ($mb MB)"

Write-Host "== Installer"
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    Write-Warning "Inno Setup 6 not found, skipping the installer (winget install JRSoftware.InnoSetup)"
    return
}
& $iscc /Q "/DAppVersion=$version" "/DSourceDir=$stage" packaging\installer.iss
if ($LASTEXITCODE -ne 0) { throw "installer build failed" }
$setup = Join-Path $PSScriptRoot "dist\MicWright-$version-Setup.exe"
$mb = [math]::Round((Get-Item $setup).Length / 1MB, 1)
Write-Host "Done: $setup ($mb MB)"
