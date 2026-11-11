# Skink installer for Windows (PowerShell 5.1+ or 7+).
#
#   irm https://skink.cc/install.ps1 | iex
#
# Downloads the Windows build, verifies its SHA-256, installs to
# %LOCALAPPDATA%\Programs\skink\bin (no admin needed) and adds that folder to your
# user PATH. Environment variables:
#   SKINK_VERSION=v0.1.0   install a specific release (default: latest)
#   SKINK_INSTALL_DIR=DIR  install somewhere else
#   SKINK_NO_MODIFY_PATH=1 do not change PATH
#   SKINK_UNINSTALL=1      remove skink and its PATH entry
#   SKINK_BASE_URL=URL     download from URL instead of GitHub (testing, mirrors)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$Repo = 'xuckless/skink'
$InstallDir = if ($env:SKINK_INSTALL_DIR) { $env:SKINK_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\skink\bin' }

function Get-UserPath { [Environment]::GetEnvironmentVariable('Path', 'User') }
function Set-UserPath([string]$v) { [Environment]::SetEnvironmentVariable('Path', $v, 'User') }
function Split-Paths([string]$p) { if ($p) { $p.Split(';') | Where-Object { $_ } } else { @() } }

if ($env:SKINK_UNINSTALL -eq '1') {
    Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $InstallDir 'skink.exe')
    $keep = Split-Paths (Get-UserPath) | Where-Object { $_.TrimEnd('\') -ne $InstallDir.TrimEnd('\') }
    Set-UserPath ($keep -join ';')
    Write-Host "skink removed from $InstallDir and PATH"
    return
}

$arch = $env:PROCESSOR_ARCHITEW6432; if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' { $target = 'x86_64-pc-windows-msvc'; Write-Host 'skink: ARM64 Windows runs the x64 build under emulation' }
    default { throw "unsupported CPU '$arch'" }
}
$asset = "skink-$target.zip"
if ($env:SKINK_BASE_URL) { $base = $env:SKINK_BASE_URL }
elseif ($env:SKINK_VERSION) { $base = "https://github.com/$Repo/releases/download/$($env:SKINK_VERSION)" }
else { $base = "https://github.com/$Repo/releases/latest/download" }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("skink-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "skink: installing for $target"
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset)
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile (Join-Path $tmp "$asset.sha256")

    $want = ((Get-Content (Join-Path $tmp "$asset.sha256") -Raw).Trim() -split '\s+')[0].ToLower()
    $got = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $asset)).Hash.ToLower()
    if ($want -ne $got) { throw "checksum mismatch for $asset (expected $want, got $got); refusing to install" }

    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    $exe = Join-Path $tmp "skink-$target\skink.exe"
    if (-not (Test-Path $exe)) { throw 'archive did not contain skink.exe' }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $dest = Join-Path $InstallDir 'skink.exe'
    # A running skink.exe cannot be overwritten, but it can be renamed out of the way.
    if (Test-Path $dest) { Move-Item -Force $dest "$dest.old" -ErrorAction SilentlyContinue }
    Copy-Item -Force $exe $dest
    Remove-Item -Force -ErrorAction SilentlyContinue "$dest.old"
    Write-Host ("skink: installed " + (& $dest --version) + " -> $dest")
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
}

if ($env:SKINK_NO_MODIFY_PATH -ne '1') {
    $have = Split-Paths (Get-UserPath) | Where-Object { $_.TrimEnd('\') -eq $InstallDir.TrimEnd('\') }
    if (-not $have) {
        $cur = Get-UserPath
        Set-UserPath ($(if ($cur) { "$cur;$InstallDir" } else { $InstallDir }))
        Write-Host "skink: added $InstallDir to your user PATH"
    }
    if (-not (Split-Paths $env:Path | Where-Object { $_.TrimEnd('\') -eq $InstallDir.TrimEnd('\') })) {
        $env:Path = "$env:Path;$InstallDir"   # works in this window right away
    }
    Write-Host 'skink: open a new terminal for other windows to see it'
}
Write-Host "skink: run  skink FILE  (or  skink .  for a folder). Ctrl-E then 'help' for the docs."
Write-Host 'skink: icons use a Nerd Font (Windows Terminal: install one, e.g. CaskaydiaCove Nerd Font); set SKINK_ICONS=ascii to turn them off.'
