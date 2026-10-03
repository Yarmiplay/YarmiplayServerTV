<#
.SYNOPSIS
  Builds the Microsoft Store package: wraps the app (npx tauri build --no-bundle) with
  src-tauri/msix/AppxManifest.xml, the logos from src-tauri/icons and the pinned Jellyfin server from
  src-tauri/jellyfin-manifest.json (downloaded once into src-tauri/target/msix/cache and checked against its
  SHA-256) into an unsigned src-tauri/target/msix/YarmiplayServerTV-<version>.msix. The Store signs it when
  it is uploaded in Partner Center. The app notices it runs from the package: it runs the built-in Jellyfin
  instead of downloading one and leaves updates to the Store.

  -Register installs the unpacked package for a local check instead (needs Developer Mode, Settings >
  System > For developers); remove it again from Settings > Apps.

.EXAMPLE
  ./scripts/make-msix.ps1
  ./scripts/make-msix.ps1 -NoBuild -Register
#>
param(
    [switch]$NoBuild,
    [switch]$Register
)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot
$tauri = Join-Path $root "src-tauri"
$version = (Get-Content "$tauri\tauri.conf.json" -Raw | ConvertFrom-Json).version
$exe = "$tauri\target\release\yarmiplayservertv.exe"
$out = "$tauri\target\msix"
$layout = "$out\layout"

if (-not $NoBuild) {
    Write-Host "==> Building the app" -ForegroundColor Cyan
    Push-Location $root
    # Not "& npx": npm's npx.ps1 rereads the command line and then finds no command to run.
    npx tauri build --no-bundle
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -ne 0) { throw "Tauri build failed." }
}
if (-not (Test-Path $exe)) { throw "No app at $exe; run without -NoBuild first." }

Write-Host "==> Laying out YarmiplayServerTV $version" -ForegroundColor Cyan
if ($Register) {
    Get-AppxPackage -Name "Yarmiplay.YarmiplayServerTV" | Where-Object IsDevelopmentMode | Remove-AppxPackage
}
if (Test-Path $layout) { Remove-Item $layout -Recurse -Force }
New-Item "$layout\Assets" -ItemType Directory | Out-Null
Copy-Item $exe $layout
foreach ($logo in "StoreLogo", "Square44x44Logo", "Square150x150Logo") {
    Copy-Item "$tauri\icons\$logo.png" "$layout\Assets\"
}
$jf = Get-Content "$tauri\jellyfin-manifest.json" -Raw | ConvertFrom-Json
$server = $jf.platforms.'windows-x86_64'.server
$zip = "$out\cache\jellyfin_$($jf.jellyfin)-amd64.zip"
$valid = (Test-Path $zip) -and (Get-FileHash $zip -Algorithm SHA256).Hash -eq $server.sha256
if (-not $valid) {
    Write-Host "==> Downloading Jellyfin $($jf.jellyfin)" -ForegroundColor Cyan
    New-Item "$out\cache" -ItemType Directory -Force | Out-Null
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest $server.url -OutFile $zip -UseBasicParsing
    if ((Get-FileHash $zip -Algorithm SHA256).Hash -ne $server.sha256) {
        Remove-Item $zip
        throw "Checksum mismatch for $($server.url)"
    }
}
Write-Host "==> Adding Jellyfin $($jf.jellyfin)" -ForegroundColor Cyan
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::ExtractToDirectory($zip, "$layout\jellyfin")
$utf8 = New-Object System.Text.UTF8Encoding($false)
$notice = @"
Jellyfin $($jf.jellyfin) for Windows (x64), unmodified, as published at
$($server.url)

Jellyfin is free software under the GNU General Public License, version 2; the ffmpeg it includes
(jellyfin-ffmpeg $($jf.ffmpeg)) is under the GNU General Public License, version 3. Their source code:
https://github.com/jellyfin/jellyfin/tree/v$($jf.jellyfin)
https://github.com/jellyfin/jellyfin-web/tree/v$($jf.jellyfin)
https://github.com/jellyfin/jellyfin-ffmpeg/tree/v$($jf.ffmpeg)
"@
[System.IO.File]::WriteAllText("$layout\jellyfin\NOTICE.txt", $notice.Replace("`r`n", "`n").Replace("`n", "`r`n"), $utf8)
$manifest = (Get-Content "$tauri\msix\AppxManifest.xml" -Raw).Replace("{VERSION}", $version)
[System.IO.File]::WriteAllText("$layout\AppxManifest.xml", $manifest, $utf8)

if ($Register) {
    Write-Host "==> Registering the package" -ForegroundColor Cyan
    Add-AppxPackage -Register "$layout\AppxManifest.xml" -ForceApplicationShutdown
    Write-Host "Installed; start YarmiplayServerTV from the Start menu."
    return
}

$makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\makeappx.exe" -ErrorAction SilentlyContinue |
    Where-Object { $_.Directory.Parent.Name -match '^\d+(\.\d+)+$' } | Sort-Object { [version]$_.Directory.Parent.Name } -Descending | Select-Object -First 1
if (-not $makeappx) { throw "makeappx.exe not found; install the Windows SDK." }

$msix = "$out\YarmiplayServerTV-$version.msix"
Write-Host "==> Packing $msix" -ForegroundColor Cyan
& $makeappx.FullName pack /d $layout /p $msix /o
if ($LASTEXITCODE -ne 0) { throw "makeappx failed." }
$msix
