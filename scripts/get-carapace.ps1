# get-carapace.ps1 - the scripted carapace acquisition (Part03 3.5.1).
#
# Downloads the carapace-bin release zip for this platform, verifies its
# SHA-256 against the release's checksums file, and unpacks carapace.exe
# into the repository's bin/ directory. Replaces the hand-downloaded
# binary: a fresh clone plus this script is a complete environment.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/get-carapace.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/get-carapace.ps1 -Version 1.8.1
param(
    # The carapace-bin release to fetch (without the leading "v").
    [string]$Version = "1.8.1"
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

# The repository root (this script lives in <repo>/scripts).
$repoRoot = Split-Path -Parent $PSScriptRoot
$destDir = Join-Path $repoRoot "bin"

$asset = "carapace-bin_${Version}_windows_amd64.zip"
$checksumsName = "carapace-bin_${Version}_checksums.txt"
$baseUrl = "https://github.com/carapace-sh/carapace-bin/releases/download/v${Version}"
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) "agony-carapace-${Version}"

function Get-ReleaseFile([string]$url, [string]$target) {
    Write-Host "downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $target -UseBasicParsing
}

function Get-ExpectedHash([string]$checksumsPath, [string]$fileName) {
    $lines = Get-Content $checksumsPath
    foreach ($line in $lines) {
        $parts = $line -split '\s+', 2
        if ($parts.Length -eq 2 -and $parts[1].Trim() -eq $fileName) {
            return $parts[0].ToLower()
        }
    }
    throw "checksums file does not list '$fileName' - refusing to continue"
}

try {
    if (-not (Test-Path $tempDir)) { New-Item -ItemType Directory -Path $tempDir | Out-Null }

    $zipPath = Join-Path $tempDir $asset
    $checksumsPath = Join-Path $tempDir $checksumsName
    Get-ReleaseFile "$baseUrl/$asset" $zipPath
    Get-ReleaseFile "$baseUrl/$checksumsName" $checksumsPath

    $expected = Get-ExpectedHash $checksumsPath $asset
    $actual = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash.ToLower()
    if ($actual -ne $expected) {
        throw "checksum mismatch for ${asset}: expected $expected, got $actual"
    }
    Write-Host "checksum OK ($actual)"

    if (-not (Test-Path $destDir)) { New-Item -ItemType Directory -Path $destDir | Out-Null }
    $staging = Join-Path $tempDir "unpacked"
    if (Test-Path $staging) { Remove-Item -Recurse -Force $staging }
    Expand-Archive -Path $zipPath -DestinationPath $staging -Force
    $exe = Get-ChildItem -Path $staging -Recurse -Filter "carapace.exe" | Select-Object -First 1
    if (-not $exe) { throw "the zip does not contain carapace.exe" }
    Copy-Item -Path $exe.FullName -Destination (Join-Path $destDir "carapace.exe") -Force

    Write-Host "installed $(Join-Path $destDir 'carapace.exe')"
    & (Join-Path $destDir "carapace.exe") --version
    exit 0
} finally {
    if (Test-Path $tempDir) { Remove-Item -Recurse -Force $tempDir -ErrorAction SilentlyContinue }
}
