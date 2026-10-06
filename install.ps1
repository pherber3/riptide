# Builds Riptide and installs it to D:\Apps\Riptide, beside its data and cache. Close the app first.
$ErrorActionPreference = 'Stop'
$env:RUSTUP_HOME = 'D:\dev\rust\rustup'
$env:CARGO_HOME = 'D:\dev\rust\cargo'
$env:PATH = "D:\dev\rust\cargo\bin;$env:PATH"
cargo build --release --manifest-path "$PSScriptRoot\Cargo.toml"
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$dir = 'D:\Apps\Riptide'
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item "$PSScriptRoot\target\release\riptide.exe" $dir -Force
$link = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Riptide.lnk")
$link.TargetPath = "$dir\riptide.exe"
$link.WorkingDirectory = $dir
$link.Save()
