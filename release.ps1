<#
.SYNOPSIS
    Build, publish, and launch a release of Scrim. One command.

.DESCRIPTION
    The button. It does the whole thing:

      1. checks the working tree is clean and the version is not already out
      2. runs the test suite
      3. builds the installer and the portable folder
      4. tags, pushes, and publishes the GitHub release with both artifacts
      5. launches the build it just published, so you see what shipped

    Stops at the first thing that looks wrong rather than publishing something
    nobody has run.

.PARAMETER Version
    Defaults to the version in Cargo.toml. Pass one to bump: -Version 0.2.0
    rewrites Cargo.toml and tauri.conf.json and commits the change.

.PARAMETER Draft
    Publish as a draft so you can look at it before it is public.

.PARAMETER NoPublish
    Build and launch only. Nothing is tagged, pushed, or published.

.PARAMETER NoRun
    Skip launching at the end.

.EXAMPLE
    .\release.ps1                      # release the current version and run it
    .\release.ps1 -Version 0.2.0       # bump, release, run
    .\release.ps1 -NoPublish           # just build and run what is here
    .\release.ps1 -Draft               # publish as a draft first
#>
[CmdletBinding()]
param(
    [string]$Version,
    [switch]$Draft,
    [switch]$NoPublish,
    [switch]$NoRun,
    [switch]$SkipTests,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$repo = $PSScriptRoot
Set-Location $repo

function Step($text) { Write-Host "`n=== $text ===" -ForegroundColor Cyan }
function Ok($text) { Write-Host "  $text" -ForegroundColor Green }
function Note($text) { Write-Host "  $text" -ForegroundColor DarkGray }
function Warn($text) { Write-Host "  $text" -ForegroundColor Yellow }

# ---------------------------------------------------------------- checks ---

Step "checks"

if (-not (Get-Command git -ErrorAction SilentlyContinue)) { throw "git is not on PATH" }

$currentVersion = (Select-String -Path "$repo\Cargo.toml" -Pattern '^version\s*=\s*"(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $Version) { $Version = $currentVersion }
$tag = "v$Version"
Note "version $Version  ->  tag $tag"

if (-not $NoPublish) {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw "the GitHub CLI (gh) is not on PATH" }
    gh auth status 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "gh is not logged in. Run: gh auth login" }

    $existing = gh release view $tag --json tagName 2>$null
    if ($existing -and -not $Force) {
        throw "$tag is already released. Bump with -Version, or pass -Force to replace it."
    }
}

# A release built from uncommitted work is not reproducible from the tag.
$dirty = git status --porcelain
if ($dirty -and -not $Force -and $Version -eq $currentVersion) {
    Write-Host $dirty
    throw "the working tree has uncommitted changes. Commit them, or pass -Force."
}

# ------------------------------------------------------------ version bump -

if ($Version -ne $currentVersion) {
    Step "bumping $currentVersion -> $Version"

    $cargo = Get-Content "$repo\Cargo.toml" -Raw
    $cargo = $cargo -replace '(?m)^version\s*=\s*"' + [regex]::Escape($currentVersion) + '"', "version = `"$Version`""
    Set-Content "$repo\Cargo.toml" $cargo -Encoding utf8 -NoNewline

    $conf = Get-Content "$repo\src-tauri\tauri.conf.json" -Raw
    $conf = $conf -replace '"version"\s*:\s*"' + [regex]::Escape($currentVersion) + '"', "`"version`": `"$Version`""
    Set-Content "$repo\src-tauri\tauri.conf.json" $conf -Encoding utf8 -NoNewline

    # Keep Cargo.lock in step so the commit is complete.
    . "$repo\tools\msvc-env.ps1"
    cargo update -w --offline 2>&1 | Out-Null

    git add Cargo.toml Cargo.lock src-tauri/tauri.conf.json
    git commit -q -m "Release $Version"
    Ok "bumped and committed"
}

# ------------------------------------------------------------------ build --

Step "build"
$packageArgs = @()
if ($SkipTests) { $packageArgs += "-SkipTests" }
& "$repo\tools\package.ps1" @packageArgs
if ($LASTEXITCODE -ne 0) { throw "packaging failed" }

$installer = Get-ChildItem "$repo\dist" -Filter "Scrim_${Version}_x64-setup.exe" -ErrorAction SilentlyContinue |
    Select-Object -First 1
$portableZip = Get-ChildItem "$repo\dist" -Filter "scrim-$Version-windows-x64-portable.zip" -ErrorAction SilentlyContinue |
    Select-Object -First 1

if (-not $portableZip) { throw "no portable zip in dist/ for $Version" }
if (-not $installer) { Warn "no installer built; releasing the portable zip only" }

# --------------------------------------------------------------- publish ---

if (-not $NoPublish) {
    Step "publish"

    $notesPath = "$repo\docs\release-notes\$tag.md"
    if (-not (Test-Path $notesPath)) {
        # Fall back to the commit subjects since the previous tag.
        $prev = git describe --tags --abbrev=0 "$tag^" 2>$null
        $range = if ($prev) { "$prev..HEAD" } else { "HEAD" }
        $log = git log $range --format="- %s" --no-merges
        $notesPath = Join-Path $env:TEMP "scrim-notes-$Version.md"
        @"
Scrim $Version.

Windows 10 or 11, 64-bit. The installer is the usual choice; the portable zip
installs nothing. Both carry their own mpv, ffmpeg, ONNX Runtime and detection
model, so nothing else is needed.

Everything runs on your machine. No uploads, no telemetry.

## Changes

$($log -join "`n")

Licensed AGPL-3.0-or-later. Bundled components are listed in THIRD-PARTY.md.
"@ | Set-Content $notesPath -Encoding utf8
        Note "generated notes from the commit log"
    }
    else {
        Note "using docs/release-notes/$tag.md"
    }

    git push -q origin HEAD
    if (git tag -l $tag) {
        if ($Force) { git tag -d $tag | Out-Null; git push -q --delete origin $tag 2>$null }
    }
    git tag -a $tag -m "Scrim $Version"
    git push -q origin $tag
    Ok "tagged and pushed $tag"

    $assets = @($portableZip.FullName)
    if ($installer) { $assets += $installer.FullName }

    $releaseArgs = @("release", "create", $tag, "--title", "Scrim $Version", "--notes-file", $notesPath)
    if ($Draft) { $releaseArgs += "--draft" }
    if ($Force) { gh release delete $tag --yes --cleanup-tag 2>$null | Out-Null; git push -q origin $tag }
    $releaseArgs += $assets

    & gh @releaseArgs
    if ($LASTEXITCODE -ne 0) { throw "gh release create failed" }

    $url = gh release view $tag --json url --jq .url
    Ok "published: $url"
}

# ------------------------------------------------------------------- run ---

if (-not $NoRun) {
    Step "launching what was just built"

    # Run the unpacked portable folder, which is the artifact people download,
    # rather than target\release\scrim.exe. If resources were left out of the
    # package this is where it shows.
    $folder = Get-ChildItem "$repo\dist" -Directory -Filter "scrim-$Version-windows-x64*" |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    $exe = Join-Path $folder.FullName "scrim.exe"
    if (-not (Test-Path $exe)) { throw "no scrim.exe in $($folder.FullName)" }

    Get-Process scrim, mpv -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400
    $proc = Start-Process $exe -PassThru
    Start-Sleep -Seconds 6
    if ($proc.HasExited) {
        throw "it exited immediately with $($proc.ExitCode). The published build does not start."
    }
    Ok "running from $($folder.Name)  (pid $($proc.Id))"
}

Step "done"
Note "installer  : $(if ($installer) { $installer.Name } else { 'not built' })"
Note "portable   : $($portableZip.Name)"
if (-not $NoPublish) { Note "release    : $tag" }
