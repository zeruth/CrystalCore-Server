# Runs crystal-server (its console) with the β install's game data, from the repo root.
# Extra arguments go to the server, e.g. -- --plain.
param([string]$Game = "C:/Users/tyler/Documents/code/CrystalCore/archive/installs/beta-672261/game")

$env:CRYSTAL_DATA__GAME = $Game
# Some tools (coding agents among them) set NO_COLOR, which turns the console's colors off.
$env:NO_COLOR = $null
Set-Location (Split-Path $PSScriptRoot -Parent)
# always the current code
cargo build -p crystal-server -j 2
if ($LASTEXITCODE -ne 0) { Write-Host "build failed"; exit 1 }
& .\target\debug\crystal-server.exe @args
