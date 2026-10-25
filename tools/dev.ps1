<#
Development services on Windows, from portable builds kept in .dev/ (gitignored).

    tools/dev.ps1 install   download Redis and PostgreSQL, create the cluster and databases
    tools/dev.ps1 start     start both
    tools/dev.ps1 stop      stop both
    tools/dev.ps1 status

Redis listens on 127.0.0.1:6379, PostgreSQL on 127.0.0.1:5432 (user `crystal`, trust auth,
databases crystal_login, crystal_characters, crystal_world). Local development only.
#>
param([Parameter(Mandatory)][ValidateSet('install', 'start', 'stop', 'status')][string]$Command)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$RedisVersion = '8.10.2'
$PgVersion = '18.4-1'

$Root = Split-Path $PSScriptRoot -Parent
$Dev = Join-Path $Root '.dev'
$RedisDir = Join-Path $Dev 'redis'
$PgDir = Join-Path $Dev 'pgsql'
$PgData = Join-Path $Dev 'pgdata'
$PgLog = Join-Path $Dev 'postgres.log'
$Databases = 'crystal_login', 'crystal_characters', 'crystal_world'

function RedisExe { Get-ChildItem $RedisDir -Recurse -Filter redis-server.exe | Select-Object -First 1 -ExpandProperty FullName }
function RedisCli { Get-ChildItem $RedisDir -Recurse -Filter redis-cli.exe | Select-Object -First 1 -ExpandProperty FullName }
function Pg([string]$tool) { Join-Path $PgDir "bin\$tool.exe" }

function Fetch([string]$url, [string]$dest) {
    $zip = Join-Path $Dev ([IO.Path]::GetFileName($url))
    if (-not (Test-Path $zip)) {
        Write-Host "downloading $url"
        Invoke-WebRequest $url -OutFile $zip -UseBasicParsing
    }
    Write-Host "extracting $zip"
    Expand-Archive $zip -DestinationPath $dest -Force
}

function Invoke-Install {
    New-Item -ItemType Directory -Force $Dev | Out-Null
    if (-not (Test-Path $RedisDir)) {
        Fetch "https://github.com/redis-windows/redis-windows/releases/download/$RedisVersion/Redis-$RedisVersion-Windows-x64-msys2.zip" $RedisDir
    }
    if (-not (Test-Path $PgDir)) {
        # The zip holds a top-level pgsql/ folder.
        Fetch "https://get.enterprisedb.com/postgresql/postgresql-$PgVersion-windows-x64-binaries.zip" $Dev
    }
    if (-not (Test-Path $PgData)) {
        & (Pg initdb) -D $PgData -U crystal --auth=trust --encoding=UTF8 --locale=C | Out-Host
    }
    Start-Services
    foreach ($db in $Databases) {
        $exists = & (Pg psql) -h 127.0.0.1 -U crystal -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname = '$db'"
        if ($exists -ne '1') { & (Pg createdb) -h 127.0.0.1 -U crystal $db; Write-Host "created $db" }
    }
}

function RedisUp { try { (& (RedisCli) -h 127.0.0.1 -p 6379 ping 2>$null) -eq 'PONG' } catch { $false } }
function PgUp { & (Pg pg_ctl) status -D $PgData *> $null; $LASTEXITCODE -eq 0 }

function Start-Services {
    if (-not (RedisUp)) {
        Start-Process (RedisExe) -ArgumentList '--bind', '127.0.0.1', '--port', '6379', '--save', '""', '--appendonly', 'no' -WindowStyle Hidden
        Write-Host 'redis started'
    }
    if (-not (PgUp)) {
        # In its own hidden console, waiting for pg_ctl alone: the server inherits pg_ctl's handles,
        # and both a captured pipe and Start-Process -Wait (which waits for descendants) would keep
        # this script waiting for as long as the server runs.
        $p = Start-Process (Pg pg_ctl) -WindowStyle Hidden -PassThru -ArgumentList 'start', '-D', "`"$PgData`"", '-l', "`"$PgLog`"", '-w', '-o', '"-c listen_addresses=127.0.0.1 -c port=5432"'
        $p.WaitForExit()
        Write-Host 'postgres started'
    }
    Show-Status
}

function Stop-Services {
    if (RedisUp) { & (RedisCli) -h 127.0.0.1 -p 6379 shutdown nosave 2>$null | Out-Null; Write-Host 'redis stopped' }
    if (PgUp) { & (Pg pg_ctl) stop -D $PgData -m fast | Out-Host }
}

function Show-Status {
    Write-Host ("redis:    " + $(if (RedisUp) { 'up (127.0.0.1:6379)' } else { 'down' }))
    Write-Host ("postgres: " + $(if (PgUp) { 'up (127.0.0.1:5432)' } else { 'down' }))
}

switch ($Command) {
    'install' { Invoke-Install }
    'start' { Start-Services }
    'stop' { Stop-Services }
    'status' { Show-Status }
}
