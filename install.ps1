# Installs athena-connect on Windows (PowerShell):
#   irm https://raw.githubusercontent.com/OhPorty/athena-connect/main/install.ps1 | iex
# Downloads the latest release, checks it against the release's SHA256SUMS,
# puts it in %LOCALAPPDATA%\athena-connect and adds that folder to your PATH.
$ErrorActionPreference = "Stop"

$Repo = "OhPorty/athena-connect"
$Dir = if ($env:ATHENA_CONNECT_DIR) { $env:ATHENA_CONNECT_DIR } else { Join-Path $env:LOCALAPPDATA "athena-connect" }

if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") {
    Write-Error "athena-connect: there's no build for $($env:PROCESSOR_ARCHITECTURE) Windows yet."
}
$Asset = "athena-connect-windows-x86_64.exe"
$Base = "https://github.com/$Repo/releases/latest/download"

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("athena-connect-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Write-Host "Downloading $Asset..."
    Invoke-WebRequest -UseBasicParsing "$Base/$Asset" -OutFile (Join-Path $Tmp $Asset)
    Invoke-WebRequest -UseBasicParsing "$Base/SHA256SUMS" -OutFile (Join-Path $Tmp "SHA256SUMS")

    $Line = Get-Content (Join-Path $Tmp "SHA256SUMS") | Where-Object { $_ -match " $([regex]::Escape($Asset))$" } | Select-Object -First 1
    $Expected = if ($Line) { ($Line -split " ")[0].ToLower() } else { "" }
    $Actual = (Get-FileHash -Algorithm SHA256 (Join-Path $Tmp $Asset)).Hash.ToLower()
    if (-not $Expected -or $Expected -ne $Actual) {
        Write-Error "athena-connect: the download doesn't match the release's checksum; not installing it."
    }

    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    Copy-Item -Force (Join-Path $Tmp $Asset) (Join-Path $Dir "athena-connect.exe")
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not (($UserPath -split ";") -contains $Dir)) {
    $NewPath = if ($UserPath) { "$($UserPath.TrimEnd(';'));$Dir" } else { $Dir }
    [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
    $env:Path = "$env:Path;$Dir"
    Write-Host "Added $Dir to your PATH (new terminals pick it up)."
}
Write-Host "Installed $(& (Join-Path $Dir 'athena-connect.exe') --version) to $Dir"
Write-Host ""
Write-Host "Next:"
Write-Host "  athena-connect login https://your-athena     (once: pairs this computer)"
Write-Host "  cd C:\the\folder; athena-connect             (gives Athena that folder until you close the terminal)"
