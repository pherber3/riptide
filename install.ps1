# Builds Riptide from source and installs it to a folder, with a Start menu shortcut. Its data and
# cache are kept in the same folder. Close Riptide first.
#
#   .\install.ps1                      # to %LOCALAPPDATA%\Programs\Riptide
#   .\install.ps1 -Dir D:\Apps\Riptide
param([string]$Dir = "$env:LOCALAPPDATA\Programs\Riptide")
$ErrorActionPreference = 'Stop'
cargo build --release --manifest-path "$PSScriptRoot\Cargo.toml"
if ($LASTEXITCODE) { exit $LASTEXITCODE }
New-Item -ItemType Directory -Force $Dir | Out-Null
Copy-Item "$PSScriptRoot\target\release\riptide.exe" $Dir -Force
$link = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Riptide.lnk")
$link.TargetPath = "$Dir\riptide.exe"
$link.WorkingDirectory = $Dir
$link.Save()
