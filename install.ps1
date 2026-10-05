# clipr installer for Windows: downloads a prebuilt release, no Rust needed.
#
#   irm https://raw.githubusercontent.com/Mudales/clipr/master/install.ps1 | iex
#
# Uninstall:
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Mudales/clipr/master/install.ps1))) -Uninstall
#
# Set $env:CLIPR_VERSION = "v0.5.0" to pin a version (default: latest).
param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # makes Invoke-WebRequest much faster

$repo = 'Mudales/clipr'
$dir  = Join-Path $env:LOCALAPPDATA 'Programs\clipr'
$exe  = Join-Path $dir 'clipr.exe'
$run  = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

function Say($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

function Stop-Clipr {
    Get-Process clipr -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400
}

if ($Uninstall) {
    Stop-Clipr
    Remove-ItemProperty -Path $run -Name clipr -ErrorAction SilentlyContinue
    Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
    Say "Removed clipr (history and settings kept in $env:APPDATA\clipr)"
    return
}

$asset = 'clipr-windows-x86_64.zip'
$url = if ($env:CLIPR_VERSION) {
    "https://github.com/$repo/releases/download/$($env:CLIPR_VERSION)/$asset"
} else {
    "https://github.com/$repo/releases/latest/download/$asset"
}

Say 'Downloading clipr for Windows'
$zip = Join-Path ([IO.Path]::GetTempPath()) "clipr-$([guid]::NewGuid()).zip"
Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing

Stop-Clipr
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Expand-Archive -Path $zip -DestinationPath $dir -Force
Remove-Item $zip -Force
Unblock-File $exe   # no "downloaded from the internet" warning

Say 'Starting clipr at login'
Set-ItemProperty -Path $run -Name clipr -Value "`"$exe`""
Start-Process $exe

Write-Host ''
Write-Host "clipr is installed and running ($exe)."
Write-Host ''
Write-Host '  - Press Ctrl+Shift+V or click the tray icon to open it (change the shortcut in Settings).'
Write-Host '  - Win+V stays Windows'' own clipboard history.'
Write-Host '  - clipr can''t paste into apps running as administrator unless it runs as administrator too.'
