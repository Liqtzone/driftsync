$ErrorActionPreference = "Stop"

$Repo = if ($env:DRIFTSYNC_REPO) { $env:DRIFTSYNC_REPO } else { "Liqtzone/driftsync" }
$Version = if ($env:DRIFTSYNC_VERSION) { $env:DRIFTSYNC_VERSION } else { "v0.1.0" }
$InstallDir = if ($env:DRIFTSYNC_INSTALL_DIR) { $env:DRIFTSYNC_INSTALL_DIR } else { "$HOME\.local\bin" }

$Target = "x86_64-pc-windows-msvc"
$Archive = "driftsync-$Version-$Target.zip"
$Url = "https://github.com/$Repo/releases/download/$Version/$Archive"

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null

try {
    Write-Host "Downloading: $Url"
    $ZipPath = Join-Path $TmpDir $Archive
    Invoke-WebRequest -Uri $Url -OutFile $ZipPath
    Expand-Archive -Path $ZipPath -DestinationPath $TmpDir -Force

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item (Join-Path $TmpDir "driftsync-$Version-$Target\driftsync.exe") (Join-Path $InstallDir "driftsync.exe") -Force
    Write-Host "✓ Installed driftsync to $InstallDir\driftsync.exe" -ForegroundColor Green
} finally {
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}
