# Contributing

## Local Setup

```powershell
cargo test
powershell -ExecutionPolicy Bypass -File .\scripts\verify.ps1
```

Use the sample configuration for local development:

```powershell
$env:TRUST_ROUTER_CONFIG = "$PWD\sidecar.config.example.json"
cargo run --bin trust-router-sidecar -- 127.0.0.1:7878 sidecar-audit.jsonl
```

## Required Checks

Run these before submitting a change:

```powershell
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test
powershell -ExecutionPolicy Bypass -File .\scripts\verify.ps1
```

## Project Conventions

- The router must deterministically route or explicitly escalate. Never silently drop a failure.
- `Degraded` and `HalfOpen` nodes stay feasible with penalties. `Open` nodes are excluded.
- Cache invalidation is node-scoped for health changes and complete for graph or cost changes.
- The planner wraps router decisions; it must not maintain a second route cache or call an LLM implicitly.
- Persist route and plan audit events before sidecar responses.
- Keep requests, plan steps, cache history, latency samples, and recovery suggestions bounded.
- Do not log API keys, authorization headers, or sensitive tool payloads.
- Document only verified behavior. Label future capabilities **PLANNED**.