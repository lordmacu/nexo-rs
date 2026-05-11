# nexo-rs installer for Windows (PowerShell) — served at
# https://lordmacu.github.io/nexo-rs/install.ps1
#
# Usage:
#   irm https://lordmacu.github.io/nexo-rs/install.ps1 | iex
#
# Or, to pass options when running the file directly:
#   .\install.ps1 -InstallDir C:\tools\nexo -NoPlugins
#
# When invoked via `irm | iex` (no args), set options through env vars:
#   $env:NEXO_INSTALL_DIR = 'C:\tools\nexo'
#   $env:NEXO_NO_PLUGINS  = '1'   # skip the bundled plugins + persona
#   $env:NEXO_NO_PERSONA  = '1'   # skip just the default persona
#   $env:NEXO_FROM_SOURCE = '1'   # cargo install instead of the prebuilt
#   $env:NEXO_PLUGINS     = 'nexo-plugin-whatsapp nexo-plugin-telegram'  # custom set
#   $env:NEXO_PERSONA     = 'lordmacu/nexo-persona-cody'  # or '' to skip
#
# What it does:
#   1. Downloads the prebuilt `nexo-rs-x86_64-pc-windows-msvc.zip` from
#      the latest GitHub release, verifies its sha256, extracts nexo.exe,
#      drops it under your user dir, and adds that dir to your user PATH.
#   2. Falls back to `cargo install nexo-rs` (crates.io) if there's no
#      pre-built binary for this arch.
#   3. Installs the bundled channel plugins (whatsapp, telegram, email,
#      browser) + nexo-plugin-admin + a default persona — each via
#      `nexo plugin install` when a prebuilt tarball matches this arch,
#      otherwise `cargo install <crate>`. Best-effort; never fatal.
#      Skip with -NoPlugins, or just the persona with -NoPersona.

param(
    [string]$InstallDir = $env:NEXO_INSTALL_DIR,
    [switch]$NoPlugins,
    [switch]$NoPersona,
    [switch]$FromSource
)

$ErrorActionPreference = 'Stop'
$Repo     = 'lordmacu/nexo-rs'
$Releases = "https://github.com/$Repo/releases"

if (-not $NoPlugins -and $env:NEXO_NO_PLUGINS) { $NoPlugins = $true }
if (-not $NoPersona -and $env:NEXO_NO_PERSONA) { $NoPersona = $true }
if (-not $FromSource -and $env:NEXO_FROM_SOURCE) { $FromSource = $true }

$Plugins = if ($env:NEXO_PLUGINS) { $env:NEXO_PLUGINS -split '\s+' }
           else { @('nexo-plugin-whatsapp','nexo-plugin-telegram','nexo-plugin-email','nexo-plugin-browser') }
$Persona = if ($null -ne $env:NEXO_PERSONA) { $env:NEXO_PERSONA } else { 'lordmacu/nexo-persona-cody' }
if ($NoPlugins) { $Persona = '' }
if ($NoPersona) { $Persona = '' }

function Banner {
@"

  ███╗   ██╗███████╗██╗  ██╗ ██████╗
  ████╗  ██║██╔════╝╚██╗██╔╝██╔═══██╗
  ██╔██╗ ██║█████╗   ╚███╔╝ ██║   ██║
  ██║╚██╗██║██╔══╝   ██╔██╗ ██║   ██║
  ██║ ╚████║███████╗██╔╝ ██╗╚██████╔╝
  ╚═╝  ╚═══╝╚══════╝╚═╝  ╚═╝ ╚═════╝

  agent framework · installer · https://lordmacu.github.io/nexo-rs/
─────────────────────────────────────────────────────────────
"@ | Write-Host
}

function Have($name) { $null -ne (Get-Command $name -ErrorAction SilentlyContinue) }

function Detect-Target {
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
    switch ($arch) {
        'AMD64' { 'x86_64-pc-windows-msvc' }
        'ARM64' { '' }            # nexo-rs does not ship an aarch64-windows build yet
        default { '' }
    }
}

function Resolve-InstallDir {
    if ($InstallDir) { return $InstallDir }
    if (Have cargo) {
        $ch = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
        return (Join-Path $ch 'bin')
    }
    return (Join-Path $env:LOCALAPPDATA 'nexo\bin')
}

function Add-ToUserPath($dir) {
    $cur = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (($cur -split ';') -notcontains $dir) {
        [Environment]::SetEnvironmentVariable('Path', "$dir;$cur", 'User')
        Write-Host "  added $dir to your user PATH (open a new terminal to pick it up)"
    }
    if (($env:Path -split ';') -notcontains $dir) { $env:Path = "$dir;$env:Path" }
}

function Install-FromBinary {
    $target = Detect-Target
    if (-not $target) { return $false }
    $zip = "nexo-rs-$target.zip"
    $url = "$Releases/latest/download/$zip"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("nexo-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    try {
        Write-Host "→ downloading $zip from the latest release …"
        try { Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp $zip) -UseBasicParsing }
        catch { Write-Host "  download failed"; return $false }

        # sha256 sidecar — verify if present.
        try {
            Invoke-WebRequest -Uri "$url.sha256" -OutFile (Join-Path $tmp "$zip.sha256") -UseBasicParsing
            $want = ((Get-Content (Join-Path $tmp "$zip.sha256") -Raw).Trim() -split '\s+')[0]
            $got  = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $zip)).Hash
            if ($want -and ($want -ne $got)) {
                Write-Error "sha256 mismatch for $zip`n  expected $want`n  got      $got"; return $false
            }
            if ($want) { Write-Host "✓ sha256 verified" }
        } catch { }

        Write-Host "→ extracting …"
        Expand-Archive -Path (Join-Path $tmp $zip) -DestinationPath $tmp -Force
        $bin = Get-ChildItem -Path $tmp -Recurse -Filter 'nexo.exe' -File | Select-Object -First 1
        if (-not $bin) { Write-Error "no nexo.exe inside the archive"; return $false }

        $dir = Resolve-InstallDir
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        Copy-Item $bin.FullName (Join-Path $dir 'nexo.exe') -Force
        $v = try { & (Join-Path $dir 'nexo.exe') --version } catch { 'nexo' }
        Write-Host "✓ installed: $(Join-Path $dir 'nexo.exe')  ($v)"
        Add-ToUserPath $dir
        return $true
    } finally { Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue }
}

function Install-FromCargo {
    if (-not (Have cargo)) {
        Write-Error @"
no pre-built binary for this arch and ``cargo`` is not on PATH.

Install the Rust toolchain — https://rustup.rs — then re-run this script.
Or grab a package directly: $Releases/latest
"@
        return $false
    }
    Write-Host "→ cargo install nexo-rs   (from crates.io)"
    & cargo install nexo-rs
    if ($LASTEXITCODE -eq 0) { return $true }
    Write-Host "  crates.io path failed — trying the git source …"
    & cargo install --git "https://github.com/$Repo" nexo-rs
    return ($LASTEXITCODE -eq 0)
}

function Nexo-Bin {
    $dir = Resolve-InstallDir
    if (Test-Path (Join-Path $dir 'nexo.exe')) { return (Join-Path $dir 'nexo.exe') }
    $c = Get-Command nexo -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    return $null
}

function Install-OnePlugin($nexo, $id) {
    Write-Host "→ $id"
    & $nexo plugin install "lordmacu/$id"
    if ($LASTEXITCODE -eq 0) { return }
    if (Have cargo) {
        Write-Host "  (no prebuilt tarball for this arch — building from crates.io …)"
        & cargo install $id
        if ($LASTEXITCODE -eq 0) { return }
    }
    Write-Host "  WARNING: skipped $id — retry later:  nexo plugin install lordmacu/$id   (or: cargo install $id)"
}

function Install-Plugins {
    if ($NoPlugins) { return }
    $nexo = Nexo-Bin
    if (-not $nexo) { Write-Host "  WARNING: can't find the freshly-installed 'nexo' — skipping plugins"; return }
    Write-Host ""
    Write-Host "─────────────────────────────────────────────────────────────"
    Write-Host "  Installing bundled plugins + persona"
    Write-Host "─────────────────────────────────────────────────────────────"
    foreach ($p in @($Plugins + 'nexo-plugin-admin')) { Install-OnePlugin $nexo $p }
    if ($Persona) {
        Write-Host "→ persona $Persona"
        & $nexo persona install $Persona
        if ($LASTEXITCODE -ne 0) { Write-Host "  WARNING: skipped persona $Persona — retry later:  nexo persona install $Persona" }
    }
}

function Next-Steps {
@"

Next:
  1. Boot the daemon — zero config required:
       nexo            # foreground
       nexo start      # background (nexo stop / nexo restart to manage it)

  2. Open the admin web UI (auto-installs nexo-plugin-admin if missing):
       nexo admin --open

  3. (Optional) Scaffold 19 documented sample YAMLs:
       nexo init

  More plugins / personas / re-run a skipped one:
       nexo plugin install lordmacu/nexo-plugin-whatsapp
       nexo persona install lordmacu/nexo-persona-cody

  Update later:
       nexo update

Docs: https://lordmacu.github.io/nexo-rs/
"@ | Write-Host
}

Banner
$ok = if ($FromSource) { Install-FromCargo } else { (Install-FromBinary) -or (Install-FromCargo) }
if (-not $ok) { exit 1 }
Install-Plugins
Next-Steps
