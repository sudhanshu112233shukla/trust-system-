$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Command)

    & $Command[0] @($Command[1..($Command.Length - 1)])
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $($Command -join ' ')"
    }
}

function Invoke-Json {
    param([string]$Uri)
    Invoke-RestMethod -Uri $Uri -Headers @{ "X-API-Key" = "trust-router-demo-key" } -TimeoutSec 10
}

Invoke-Checked docker compose up --build -d trust-router-sidecar

$deadline = (Get-Date).AddSeconds(60)
do {
    try {
        $health = Invoke-Json "http://127.0.0.1:7878/healthz"
        if ($health.ok -eq $true) { break }
    } catch {
        Start-Sleep -Seconds 1
    }
} while ((Get-Date) -lt $deadline)

if ($health.ok -ne $true) {
    throw "sidecar did not become healthy"
}

Invoke-Checked docker compose run --rm toy-agent

$auditPath = Join-Path (Get-Location) "docker-data\audit\sidecar-audit.jsonl"
if (!(Test-Path $auditPath)) {
    throw "audit file was not created at $auditPath"
}

$before = (Get-Content -LiteralPath $auditPath).Count
Invoke-Checked docker compose restart trust-router-sidecar

$deadline = (Get-Date).AddSeconds(60)
do {
    try {
        $health = Invoke-Json "http://127.0.0.1:7878/healthz"
        if ($health.ok -eq $true) { break }
    } catch {
        Start-Sleep -Seconds 1
    }
} while ((Get-Date) -lt $deadline)

if ($health.ok -ne $true) {
    throw "sidecar did not become healthy after restart"
}

$after = (Get-Content -LiteralPath $auditPath).Count
$routeAfterRestart = Invoke-Json "http://127.0.0.1:7878/route?tenant=yc-demo&start=start&goal=done"

Write-Host "audit_lines_before_restart=$before"
Write-Host "audit_lines_after_restart=$after"
Write-Host "route_after_restart=$($routeAfterRestart | ConvertTo-Json -Compress)"
Write-Host "audit_persisted=$($after -ge $before)"
Write-Host "in_memory_graph_and_health_state_persisted=false"
Write-Host "note=The audit JSONL persists through the bind mount; graph and health state are recreated in memory on sidecar restart."

