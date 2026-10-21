# Builds Jwala (Rust) and packages it into dist\:
#   Jwala.exe   standalone app
#   Jwala.vst3  VST3 bundle   (copy to C:\Program Files\Common Files\VST3)
#   Jwala.clap  CLAP plugin   (copy to C:\Program Files\Common Files\CLAP)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $env:Path = (Join-Path $env:USERPROFILE ".cargo\bin") + ";" + $env:Path }
cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$dist = Join-Path $PSScriptRoot "dist"
Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
$vst = Join-Path $dist "Jwala.vst3\Contents\x86_64-win"
New-Item -ItemType Directory -Force $vst | Out-Null
Copy-Item target\release\jwala.exe (Join-Path $dist "Jwala.exe")
Copy-Item target\release\jwala_plugin.dll (Join-Path $vst "Jwala.vst3")
Copy-Item target\release\jwala_plugin.dll (Join-Path $dist "Jwala.clap")
Get-ChildItem $dist -Recurse -File | ForEach-Object { "{0,-60} {1,8:N1} MB" -f $_.FullName.Substring($dist.Length + 1), ($_.Length / 1MB) }
