<#
.SYNOPSIS
    Import the MSVC build environment into the current PowerShell session.

.DESCRIPTION
    Rust links with MSVC on Windows, and several crates in Scrim's tree compile
    C or C++ through cc-rs, which shells out to cl.exe. rustc finds link.exe on
    its own, but cl.exe is only on PATH inside a Developer Command Prompt, so a
    plain `cargo build` fails with "failed to find tool cl.exe".

    Dot-source this before building:

        . tools/msvc-env.ps1
        cargo build

    It picks the newest vcvars64.bat on the machine and copies the environment
    that script sets into this session.
#>

$ErrorActionPreference = "Stop"

function Import-MsvcEnv {
    if ($env:SCRIM_MSVC_READY -eq "1") {
        Write-Verbose "MSVC environment already imported"
        return
    }

    $roots = @(
        "$env:ProgramFiles\Microsoft Visual Studio",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio"
    ) | Where-Object { Test-Path $_ }

    $vcvars = $roots |
        ForEach-Object { Get-ChildItem $_ -Filter "vcvars64.bat" -Recurse -ErrorAction SilentlyContinue } |
        Sort-Object FullName -Descending |
        Select-Object -First 1

    if (-not $vcvars) {
        throw @"
No vcvars64.bat found, so there is no C++ toolchain to build with.

Install the Visual Studio C++ build tools:
    winget install Microsoft.VisualStudio.2022.BuildTools ``
        --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

That installer needs elevation; accept the UAC prompt or run it from an
elevated terminal, otherwise it exits 1602 having done nothing.
"@
    }

    Write-Host "  using $($vcvars.FullName)" -ForegroundColor DarkGray

    # Run the batch file in cmd, then copy the resulting environment across.
    cmd /c "`"$($vcvars.FullName)`" >nul 2>&1 && set" | ForEach-Object {
        if ($_ -match '^([^=]+)=(.*)$') {
            Set-Item -Path "env:$($matches[1])" -Value $matches[2] -ErrorAction SilentlyContinue
        }
    }

    # Cargo lives outside the VS environment.
    $cargoBin = "$env:USERPROFILE\.cargo\bin"
    if ($env:Path -notlike "*$cargoBin*") { $env:Path = "$cargoBin;$env:Path" }

    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
        throw "vcvars64.bat ran but cl.exe is still not on PATH."
    }

    $env:SCRIM_MSVC_READY = "1"
}

Import-MsvcEnv
