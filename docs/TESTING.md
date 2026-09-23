# Testing

## Unit and property

`cargo test --lib` covers routing cost, health transitions, cache invalidation/capacity, audit bounds, recovery, configuration, rate limiting, planner request validation, planning recovery, and concurrency. `tests/routing_properties.rs` uses `proptest` to verify deterministic valid paths, open-node exclusion, disabled-edge exclusion, cache equivalence, and invalid floating-point edge rejection.

## Integration

- `tests/sidecar_auth.rs`: auth, tenant authorization, malformed input, method contract, security headers, and rate limiting.
- `tests/sidecar_metrics.rs`: route counters, cache counters, reroute semantics, escalation, planner recovery, and plan counters.
- `tests/http_chaos.rs`: 200 HTTP chaos trials uphold reroute-or-explicit-escalation.
- `tests/audit_durability.rs`: hard process-kill audit JSONL parsing.
- `tests/sidecar_config.rs`: production startup configuration safety.

## Operational checks

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\verify.ps1
cargo run --bin trust-router-loadtest
cargo run --bin trust-router-soaktest -- 300 200
cargo run --release --bin trust-router-planner-benchmark -- 20000
```

Docker validation is only valid when Docker is available locally. If unavailable, record it as `NOT VERIFIED`.