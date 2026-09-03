$ErrorActionPreference = "Stop"
$previousApiKey = $env:TRUST_ROUTER_API_KEY
$previousOtel = $env:TRUST_ROUTER_OTEL_STDOUT
$previousInterval = $env:TRUST_ROUTER_OTEL_INTERVAL_MS
$env:TRUST_ROUTER_API_KEY = "trust-router-demo-key"
$env:TRUST_ROUTER_OTEL_STDOUT = "true"
$env:TRUST_ROUTER_OTEL_INTERVAL_MS = "250"

$stdout = Join-Path $env:TEMP "trust-router-otel-stdout-$PID.log"
$stderr = Join-Path $env:TEMP "trust-router-otel-stderr-$PID.log"
$audit = Join-Path $env:TEMP "trust-router-otel-audit-$PID.jsonl"
Remove-Item $stdout,$stderr,$audit -Force -ErrorAction SilentlyContinue

$sidecar = Start-Process -FilePath ".\target\debug\trust-router-sidecar.exe" -ArgumentList @("127.0.0.1:7891", $audit) -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -WindowStyle Hidden
try {
    $deadline = (Get-Date).AddSeconds(10)
    do {
        try {
            $health = Invoke-RestMethod -Uri "http://127.0.0.1:7891/healthz" -TimeoutSec 1
            if ($health.ok -eq $true) { break }
        } catch {
            Start-Sleep -Milliseconds 100
        }
    } while ((Get-Date) -lt $deadline)

    $headers = @{ "X-API-Key" = "trust-router-demo-key" }
    Invoke-RestMethod -Headers $headers -Uri "http://127.0.0.1:7891/route?tenant=yc-demo&start=start&goal=done" | Out-Null
    Start-Sleep -Milliseconds 900
}
finally {
    if ($null -ne $sidecar -and -not $sidecar.HasExited) {
        Stop-Process -Id $sidecar.Id -Force
        Wait-Process -Id $sidecar.Id -ErrorAction SilentlyContinue
    }
    if ($null -ne $previousApiKey) { $env:TRUST_ROUTER_API_KEY = $previousApiKey } else { Remove-Item Env:\TRUST_ROUTER_API_KEY -ErrorAction SilentlyContinue }
    if ($null -ne $previousOtel) { $env:TRUST_ROUTER_OTEL_STDOUT = $previousOtel } else { Remove-Item Env:\TRUST_ROUTER_OTEL_STDOUT -ErrorAction SilentlyContinue }
    if ($null -ne $previousInterval) { $env:TRUST_ROUTER_OTEL_INTERVAL_MS = $previousInterval } else { Remove-Item Env:\TRUST_ROUTER_OTEL_INTERVAL_MS -ErrorAction SilentlyContinue }
}

$content = Get-Content $stdout -Raw -ErrorAction SilentlyContinue
if ($content -notmatch "trust_router_routes_total") {
    Write-Host "--- sidecar stdout ---"
    Write-Host $content
    Write-Host "--- sidecar stderr ---"
    Get-Content $stderr -Raw -ErrorAction SilentlyContinue | Write-Host
    throw "OpenTelemetry stdout export did not include trust_router_routes_total"
}

Write-Host "OpenTelemetry stdout metrics export verified."
