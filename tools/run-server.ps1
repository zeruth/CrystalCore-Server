# Runs crystal-server (its console) with the β install's game data, from the repo root.
# Extra arguments go to the server, e.g. -- --plain.
param([string]$Game = "C:/Users/tyler/Documents/code/CrystalCore/archive/installs/beta-672261/game")

$env:CRYSTAL_DATA__GAME = $Game
Set-Location (Split-Path $PSScriptRoot -Parent)
& .\target\debug\crystal-server.exe @args
