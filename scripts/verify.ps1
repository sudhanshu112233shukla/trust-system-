$previousTrustRouterApiKey = $env:TRUST_ROUTER_API_KEY
$env:TRUST_ROUTER_API_KEY = "trust-router-demo-key"

$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Command)

    & $Command[0] @($Command[1..($Command.Length - 1)])
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $($Command -join ' ')"
    }
}

Invoke-Checked cargo fmt --check
Invoke-Checked cargo test
Invoke-Checked cargo run --bin trust-router-demo -- demo-audit.jsonl
Invoke-Checked cargo run --bin trust-router-baseline
Invoke-Checked cargo run --bin trust-router-loadtest
Invoke-Checked cargo build --bins

$sidecar = Start-Process `
    -FilePath ".\target\debug\trust-router-sidecar.exe" `
    -ArgumentList @("127.0.0.1:7878", "sidecar-audit.jsonl") `
    -PassThru `
    -NoNewWindow

try {
    Invoke-Checked python sdk/python/example_usage.py
}
finally {
    if ($null -ne $sidecar -and -not $sidecar.HasExited) {
        Stop-Process -Id $sidecar.Id -Force
        Wait-Process -Id $sidecar.Id -ErrorAction SilentlyContinue
    }
}

$langchainSidecar = Start-Process `
    -FilePath ".\target\debug\trust-router-sidecar.exe" `
    -ArgumentList @("127.0.0.1:7879", "langchain-sidecar-audit.jsonl") `
    -PassThru `
    -NoNewWindow

try {
    $previousTrustRouterUrl = $env:TRUST_ROUTER_URL
    $env:TRUST_ROUTER_URL = "http://127.0.0.1:7879"
    Invoke-Checked python sdk/python/langchain_integration_example.py
}
finally {
    if ($null -ne $previousTrustRouterUrl) {
        $env:TRUST_ROUTER_URL = $previousTrustRouterUrl
    } else {
        Remove-Item Env:\TRUST_ROUTER_URL -ErrorAction SilentlyContinue
    }

    if ($null -ne $langchainSidecar -and -not $langchainSidecar.HasExited) {
        Stop-Process -Id $langchainSidecar.Id -Force
        Wait-Process -Id $langchainSidecar.Id -ErrorAction SilentlyContinue
    }
}

$typescriptSidecar = Start-Process `
    -FilePath ".\target\debug\trust-router-sidecar.exe" `
    -ArgumentList @("127.0.0.1:7880", "typescript-sidecar-audit.jsonl") `
    -PassThru `
    -NoNewWindow

try {
    $previousTrustRouterUrl = $env:TRUST_ROUTER_URL
    $env:TRUST_ROUTER_URL = "http://127.0.0.1:7880"
    Invoke-Checked node --experimental-strip-types sdk/typescript/exampleUsage.ts
}
finally {
    if ($null -ne $previousTrustRouterUrl) {
        $env:TRUST_ROUTER_URL = $previousTrustRouterUrl
    } else {
        Remove-Item Env:\TRUST_ROUTER_URL -ErrorAction SilentlyContinue
    }

    if ($null -ne $typescriptSidecar -and -not $typescriptSidecar.HasExited) {
        Stop-Process -Id $typescriptSidecar.Id -Force
        Wait-Process -Id $typescriptSidecar.Id -ErrorAction SilentlyContinue
    }
}


if ($null -ne $previousTrustRouterApiKey) {
    $env:TRUST_ROUTER_API_KEY = $previousTrustRouterApiKey
} else {
    Remove-Item Env:\TRUST_ROUTER_API_KEY -ErrorAction SilentlyContinue
}

Write-Host "Trust Router verification completed successfully."

