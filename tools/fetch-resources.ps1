<#
.SYNOPSIS
    Download the third-party binaries Scrim bundles.

.DESCRIPTION
    Scrim ships as a folder you can copy to any Windows machine and run, so it
    carries its own mpv, ffmpeg, ONNX Runtime, and detection model rather than
    asking the user to install anything. Those are not ours to keep in git, so
    they are fetched here instead.

    Every download is pinned to an exact version and checked against
    resources/resources.lock.json. If a hash does not match, the file is
    rejected. Run with -UpdateLock to record hashes for a new pin.

    No other tooling is required: the one .7z download is unpacked with 7-Zip's
    standalone extractor, which this script fetches itself.

.EXAMPLE
    pwsh tools/fetch-resources.ps1
    pwsh tools/fetch-resources.ps1 -UpdateLock
#>
[CmdletBinding()]
param(
    [switch]$UpdateLock,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$repo = Split-Path -Parent $PSScriptRoot
$resources = Join-Path $repo "resources"
$lockPath = Join-Path $resources "resources.lock.json"
$work = Join-Path $env:TEMP "scrim-resources"

New-Item -ItemType Directory -Force -Path $resources, $work | Out-Null

# Pinned versions. Bump deliberately, then re-run with -UpdateLock.
$MPV_TAG = "20260814"
$MPV_BUILD = "mpv-x86_64-20260814-git-7b8915bc1d"
$ONNX_VERSION = "1.20.1"
$NUDENET_TAG = "v3.4-weights"

$sources = @(
    @{
        Name    = "ffmpeg.exe"
        Why     = "frame extraction for scanning, and the cast transcode"
        Url     = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip"
        Archive = "zip"
        Member  = "ffmpeg.exe"
        MinBytes = 20MB
        License = "GPL-3.0 (BtbN gpl build)"
    },
    # No ffprobe: it is another 139 MB static binary that duplicates ffmpeg
    # entirely, and everything Scrim needs from it (duration, fps, dimensions)
    # is already in ffmpeg's own stream report.
    @{
        Name    = "mpv.exe"
        Why     = "playback, with the censor filtergraph applied live"
        Url     = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/$MPV_TAG/$MPV_BUILD.7z"
        Archive = "7z"
        Member  = "mpv.exe"
        MinBytes = 20MB
        License = "GPL-2.0-or-later"
    },
    @{
        Name    = "onnxruntime.dll"
        Why     = "running the detection model"
        Url     = "https://github.com/microsoft/onnxruntime/releases/download/v$ONNX_VERSION/onnxruntime-win-x64-$ONNX_VERSION.zip"
        Archive = "zip"
        Member  = "onnxruntime.dll"
        MinBytes = 5MB
        License = "MIT"
    },
    @{
        # 320n is what the Python nudenet package ships and what NudeDetector()
        # loads by default, so it is the model the golden fixtures were
        # generated with. Swapping in 640m would change every detection.
        #
        # Served from raw.githubusercontent rather than the release asset:
        # NudeNet's release downloads redirect anonymous callers to a GitHub
        # login page, which arrives as a 47 KB HTML file named 320n.onnx. The
        # raw path has no such gate, and its bytes hash identically to the
        # model the reference scans were produced with.
        Name    = "320n.onnx"
        Why     = "the NudeNet detector weights, matching the reference scans"
        Url     = "https://raw.githubusercontent.com/notAI-tech/NudeNet/master/nudenet/320n.onnx"
        Archive = "none"
        MinBytes = 10MB
        Sha256  = "C15D8273ADAD2D0A92F014CC69AB2D6C311A06777A55545F2C4EB46F51911F0F"
        License = "AGPL-3.0 (this is why Scrim is AGPL-3.0)"
    }
)

function Assert-NotAnErrorPage {
    <#
        A download that fails softly is worse than one that fails loudly.
        GitHub answers some anonymous release downloads with a login page,
        served with a 200 and the filename you asked for, so a 47 KB HTML
        document lands on disk called "320n.onnx" and everything downstream
        breaks in a confusing way. Catch it at the source.
    #>
    param([string]$Path, [long]$MinBytes)

    $size = (Get-Item $Path).Length
    $head = [System.IO.File]::ReadAllBytes($Path)[0..([Math]::Min(511, $size - 1))]
    $text = [System.Text.Encoding]::ASCII.GetString($head)

    if ($text -match '(?i)<!DOCTYPE html|<html[\s>]') {
        throw "the server returned an HTML page, not a file. Scrim will not ship it.`n  saved to $Path ($size bytes)"
    }
    if ($MinBytes -and $size -lt $MinBytes) {
        throw "file is only $('{0:N0}' -f $size) bytes, expected at least $('{0:N0}' -f $MinBytes). Treating this as a failed download."
    }
}

function Get-SevenZip {
    # 7zr.exe is the standalone .7z extractor: one small file, no install.
    $exe = Join-Path $work "7zr.exe"
    if (-not (Test-Path $exe)) {
        Write-Host "  fetching 7zr.exe (standalone .7z extractor)" -ForegroundColor DarkGray
        Invoke-WebRequest -Uri "https://www.7-zip.org/a/7zr.exe" -OutFile $exe -UseBasicParsing
    }
    return $exe
}

function Expand-Member {
    param($ArchivePath, $Kind, $Leaf, $Destination)

    $stage = Join-Path $work ("x-" + [IO.Path]::GetFileNameWithoutExtension($ArchivePath))
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $stage | Out-Null

    if ($Kind -eq "zip") {
        Expand-Archive -Path $ArchivePath -DestinationPath $stage -Force
    }
    elseif ($Kind -eq "7z") {
        $seven = Get-SevenZip
        & $seven x $ArchivePath "-o$stage" -y | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "7zr failed to unpack $ArchivePath" }
    }

    $hit = Get-ChildItem $stage -Recurse -Filter $Leaf -ErrorAction SilentlyContinue |
        Sort-Object Length -Descending | Select-Object -First 1
    if (-not $hit) { throw "could not find $Leaf inside $ArchivePath" }
    Copy-Item $hit.FullName $Destination -Force
}

$lock = if (Test-Path $lockPath) { Get-Content $lockPath -Raw | ConvertFrom-Json } else { [pscustomobject]@{} }
$newLock = [ordered]@{}
$downloaded = 0

foreach ($s in $sources) {
    $target = Join-Path $resources $s.Name
    $pin = $lock.PSObject.Properties[$s.Name]

    if ((Test-Path $target) -and -not $Force -and $pin) {
        if ((Get-FileHash $target -Algorithm SHA256).Hash -eq $pin.Value.sha256) {
            Write-Host ("  ok       {0,-18} {1}" -f $s.Name, $s.Why) -ForegroundColor DarkGray
            $newLock[$s.Name] = $pin.Value
            continue
        }
        Write-Host ("  stale    {0}" -f $s.Name) -ForegroundColor Yellow
    }

    Write-Host ("  fetching {0,-18} {1}" -f $s.Name, $s.Url) -ForegroundColor Cyan

    # Cache by URL, not by the leaf filename. Two different URLs can end in the
    # same name (an asset and a raw path both called 320n.onnx), and keying on
    # the leaf silently serves one when the other was asked for.
    $sha = [System.Security.Cryptography.SHA1]::Create()
    $urlKey = -join ($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($s.Url))[0..5] |
        ForEach-Object { $_.ToString("x2") })
    $archive = Join-Path $work ("$urlKey-" + (Split-Path $s.Url -Leaf))

    if (-not (Test-Path $archive)) {
        Invoke-WebRequest -Uri $s.Url -OutFile $archive -UseBasicParsing
    }

    if ($s.Archive -eq "none") {
        Copy-Item $archive $target -Force
    }
    else {
        Expand-Member -ArchivePath $archive -Kind $s.Archive -Leaf $s.Member -Destination $target
    }

    Assert-NotAnErrorPage -Path $target -MinBytes ([long]$s.MinBytes)

    $hash = (Get-FileHash $target -Algorithm SHA256).Hash

    # A hash baked into this script beats one recorded from whatever happened
    # to download first, so it is checked even under -UpdateLock.
    if ($s.Sha256 -and $hash -ne $s.Sha256) {
        Remove-Item $target -Force
        throw "$($s.Name) does not match the hash pinned in this script.`n  expected $($s.Sha256)`n  got      $hash"
    }

    if ($pin -and -not $UpdateLock -and $hash -ne $pin.Value.sha256) {
        Remove-Item $target -Force
        throw "$($s.Name) does not match the pinned hash. Refusing to use it.`n  expected $($pin.Value.sha256)`n  got      $hash"
    }

    $newLock[$s.Name] = [ordered]@{
        url     = $s.Url
        sha256  = $hash
        bytes   = (Get-Item $target).Length
        license = $s.License
    }
    $downloaded++
}

if ($UpdateLock -or -not (Test-Path $lockPath)) {
    $newLock | ConvertTo-Json -Depth 5 | Set-Content $lockPath -Encoding utf8
    Write-Host "`nwrote $lockPath" -ForegroundColor Green
}

$total = (Get-ChildItem $resources -File | Measure-Object -Property Length -Sum).Sum
Write-Host ("`n{0} file(s) fetched. resources/ is {1:N0} MB." -f $downloaded, ($total / 1MB))
Write-Host "Licences are recorded in resources.lock.json and explained in THIRD-PARTY.md."
