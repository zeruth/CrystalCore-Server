# Runs crystal-server (its console) with the β install's game data, from the repo root.
# Extra arguments go to the server, e.g. `tools/run-server.ps1 --plain` (a `--` before them is
# fine); `-Game <path>` names another install.
# ($args read by hand: a positional `param($Game)` took `-- --plain`'s `--plain` for the game data,
# and an advanced script's binding refuses `--` itself)
$Game = "C:/Users/tyler/Documents/code/CrystalCore/archive/installs/beta-672261/game"
$ServerArgs = @()
for ($i = 0; $i -lt $args.Count; $i++) {
  if ($args[$i] -eq '-Game' -and $i + 1 -lt $args.Count) { $Game = $args[++$i] }
  elseif ($args[$i] -ne '--') { $ServerArgs += $args[$i] }
}

$env:CRYSTAL_DATA__GAME = $Game
# Some tools (coding agents among them) set NO_COLOR, which turns the console's colors off.
$env:NO_COLOR = $null
Set-Location (Split-Path $PSScriptRoot -Parent)
# always the current code
cargo build -p crystal-server -j 2
if ($LASTEXITCODE -ne 0) { Write-Host "build failed"; exit 1 }
& .\target\debug\crystal-server.exe @ServerArgs
