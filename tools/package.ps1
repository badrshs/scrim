<#
.SYNOPSIS
    Build Scrim for distribution: an installer and a portable folder.

.DESCRIPTION
    Produces both, because they answer different needs. The installer is what
    most people want. The portable folder is what proves the claim on the tin:
    copy it to a machine with nothing installed and it runs.

.EXAMPLE
    powershell -File tools/package.ps1
    powershell -File tools/package.ps1 -SkipInstaller
#>
[CmdletBinding()]
param(
    [switch]$SkipInstaller,
    [switch]$SkipTests
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

. "$repo\tools\msvc-env.ps1"

# --- the bundled binaries have to be there, or the app cannot run ----------
$required = @("mpv.exe", "ffmpeg.exe", "onnxruntime.dll", "320n.onnx")
$missing = $required | Where-Object { -not (Test-Path "$repo\resources\$_") }
if ($missing) {
    throw "resources/ is missing: $($missing -join ', ').`nRun tools/fetch-resources.ps1 first."
}

if (-not $SkipTests) {
    Write-Host "`n=== tests ===" -ForegroundColor Cyan
    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { throw "tests failed; not packaging a broken build" }
}

Write-Host "`n=== release build ===" -ForegroundColor Cyan
cargo build --release -p scrim
if ($LASTEXITCODE -ne 0) { throw "release build failed" }

$version = (Select-String -Path "$repo\Cargo.toml" -Pattern '^version\s*=\s*"(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value

$dist = "$repo\dist"
$portable = "$dist\scrim-$version-windows-x64"

# Windows will refuse to delete a directory anything still holds a handle on,
# and an antivirus scan of a 280 MB folder counts. Retry, then step aside
# rather than failing a release build over a transient lock.
if (Test-Path $portable) {
    $cleared = $false
    foreach ($attempt in 1..5) {
        try {
            Remove-Item $portable -Recurse -Force -ErrorAction Stop
            $cleared = $true
            break
        }
        catch {
            Start-Sleep -Seconds 3
        }
    }
    if (-not $cleared) {
        $portable = "$portable-$(Get-Date -Format 'HHmmss')"
        Write-Host "  previous folder is locked; building into $(Split-Path $portable -Leaf)" -ForegroundColor Yellow
    }
}
New-Item -ItemType Directory -Force -Path $portable | Out-Null

# --- portable folder -------------------------------------------------------
Write-Host "`n=== portable folder ===" -ForegroundColor Cyan
Copy-Item "$repo\target\release\scrim.exe" $portable
New-Item -ItemType Directory -Force -Path "$portable\resources" | Out-Null
foreach ($f in $required) { Copy-Item "$repo\resources\$f" "$portable\resources\" }
Copy-Item "$repo\LICENSE" $portable
Copy-Item "$repo\THIRD-PARTY.md" $portable
Copy-Item "$repo\README.md" $portable

@"
Scrim $version

Run scrim.exe. Nothing needs installing: mpv, ffmpeg, the ONNX runtime and the
detection model are all in resources\ beside it.

Your library and settings are kept in %APPDATA%\app.scrim.player, not in this
folder, so a copy on a memory stick does not carry one person's movie list to
someone else's machine.

Licensed AGPL-3.0-or-later. See LICENSE and THIRD-PARTY.md.
"@ | Set-Content "$portable\READ ME FIRST.txt" -Encoding utf8

$zip = "$dist\scrim-$version-windows-x64-portable.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path "$portable\*" -DestinationPath $zip -CompressionLevel Optimal

# --- installer -------------------------------------------------------------
if (-not $SkipInstaller) {
    Write-Host "`n=== installer ===" -ForegroundColor Cyan
    # Probe for the subcommand rather than running it and reading $LASTEXITCODE:
    # in Windows PowerShell, redirecting a native command's stderr reports a
    # failure even for a run that succeeded.
    $hasTauri = (cargo --list | Select-String -Quiet '^\s+tauri\b')
    if (-not $hasTauri) {
        Write-Host "  the Tauri CLI is not installed, so no installer was built." -ForegroundColor Yellow
        Write-Host "  cargo install tauri-cli --version `"^2`" --locked" -ForegroundColor Yellow
    }
    else {
        cargo tauri build
        $nsis = Get-ChildItem "$repo\target\release\bundle\nsis" -Filter "*.exe" -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($nsis) { Copy-Item $nsis.FullName $dist }
        else { Write-Host "  no NSIS output found." -ForegroundColor Yellow }
    }
}

Write-Host "`n=== done ===" -ForegroundColor Green
Get-ChildItem $dist -File | ForEach-Object {
    "  {0,-52} {1,8:N1} MB" -f $_.Name, ($_.Length / 1MB)
}
"  {0,-52} {1,8:N1} MB" -f "$([IO.Path]::GetFileName($portable))\ (unpacked)",
    ((Get-ChildItem $portable -Recurse -File | Measure-Object Length -Sum).Sum / 1MB)
