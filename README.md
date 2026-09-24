# Trust Router

Trust Router is a deterministic planner and routing core for AI-agent tool workflows. It selects a feasible, cost-aware tool path without an LLM control-plane call. When no safe deterministic path exists, it returns an explicit bounded recovery outcome instead of silently skipping work.

## What Is Implemented

- Tenant-isolated, deterministic shortest-path routing with normalized cost weights.
- Circuit breaker states: `Healthy`, `Degraded`, `Open`, and `HalfOpen`.
- Bounded rolling p99 latency samples per node; `p99_latency_ms` is not the last observed latency.
- Node-scoped cache invalidation and a bounded per-tenant LRU route cache.
- Bounded in-memory audit history plus durable append-only JSONL audit records with `sync_all()`.
- Async health result ingestion and automatic `Open -> HalfOpen` recovery probes.
- An Axum sidecar with `/route`, `/plan`, `/result`, `/force-half-open`, `/metrics`, and `/healthz`.
- Explicit `ExecutionPlan` output: validated request, system snapshot, selected candidate, bounded steps, or bounded recovery plan.
- API-key authentication, tenant allow-list authorization, constant-time key comparison, identifier limits, request IDs, structured errors, and token-bucket rate limiting.
- Python and TypeScript sidecar clients plus executable local examples.
- Property, unit, HTTP integration, chaos, durability, load, and soak test coverage.

## Quick Start

```powershell
cargo test
cargo run --bin trust-router-demo -- demo-audit.jsonl
cargo run --bin trust-router-sidecar -- 127.0.0.1:7878 sidecar-audit.jsonl
```

The sidecar arguments are the bind address and the JSONL audit file path.

Local development defaults use `X-API-Key: trust-router-demo-key`. Set `TRUST_ROUTER_API_KEY` to choose another key.

## Planner Model

```text
request
  -> request validation
  -> system snapshot
  -> candidate generation (routing core)
  -> feasibility and score validation
  -> selection
  -> ExecutionPlan | bounded recovery
  -> SDK/execution adapter
```

The planner does not execute tools, call an LLM, or schedule GPUs. Existing SDK adapters own execution and report outcomes through `/result`. Future adapters for other engines are **PLANNED**, not implemented.

## Sidecar API

All endpoints except `/healthz` require `X-API-Key`. Tenant-specific endpoints also require the tenant to appear in `allowed_tenants`.

```text
GET  /healthz
GET  /route?tenant=yc-demo&start=start&goal=done
GET  /plan?tenant=yc-demo&start=start&goal=done
POST /result?tenant=yc-demo&node=primary_search&success=false&latency_ms=30000
POST /force-half-open?tenant=yc-demo&node=primary_search
GET  /metrics
```

`/route` returns a raw router decision. `/plan` returns either:

```json
{
  "decision": "execute",
  "schema_version": 1,
  "path": ["start", "primary_search", "summarize", "done"],
  "total_cost": 0.438,
  "cache_hit": false,
  "steps": ["start", "primary_search", "summarize", "done"]
}
```

or an explicit recovery response:

```json
{
  "decision": "recover",
  "reason": "NoFeasiblePath",
  "failed_or_blocked_nodes": ["primary_search", "fallback_search"],
  "recovery_steps": ["Inspect or replace blocked tool: primary_search"]
}
```

Errors use a stable shape:

```json
{
  "error": {
    "code": "tenant_not_authorized",
    "message": "API key is not authorized for this tenant",
    "request_id": "tr-0000000000000001"
  }
}
```

Expected status codes: `400` invalid input, `401` missing or invalid key, `403` unauthorized tenant, `405` wrong method, `429` rate limited, and `500` internal persistence failure. Every protected response includes `X-Request-Id` and `X-Content-Type-Options: nosniff`.

## Configuration

Copy [sidecar.config.example.json](sidecar.config.example.json) and set `TRUST_ROUTER_CONFIG` to its path. Environment variables override file values. Supported operational settings include:

- `TRUST_ROUTER_MODE=development|production`
- `TRUST_ROUTER_API_KEY` or `TRUST_ROUTER_API_KEY_FILE`
- `TRUST_ROUTER_ALLOWED_TENANTS`
- `TRUST_ROUTER_RATE_LIMIT_CAPACITY` and `TRUST_ROUTER_RATE_LIMIT_REFILL_PER_SECOND`
- `TRUST_ROUTER_W_DOLLAR`, `TRUST_ROUTER_W_LATENCY`, `TRUST_ROUTER_W_RISK`, `TRUST_ROUTER_W_BASE`
- `TRUST_ROUTER_OPEN_THRESHOLD`, `TRUST_ROUTER_HALF_OPEN_SUCCESS_NEEDED`, and `TRUST_ROUTER_COOLDOWN_MS`

Production mode refuses to start with no secret, the demo key, a key shorter than 32 characters, malformed numeric values, unsafe health thresholds, or a missing explicit tenant allow-list.

## Observability

`/metrics` provides bounded p50/p95/p99 samples and route, cache, reroute, escalation, false-escalation, and planner counters. A reroute is counted only when a non-cached successful path changes; it is not synonymous with every successful route.

Set `TRUST_ROUTER_OTEL_STDOUT=true` for the optional stdout OpenTelemetry exporter. Durable JSONL audit records and OTel metrics are separate: JSONL is the decision record; telemetry is for process operations.

## Verification

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\verify.ps1
cargo run --bin trust-router-loadtest
cargo run --bin trust-router-soaktest -- 300 200
cargo run --release --bin trust-router-planner-benchmark -- 20000
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test
```

See [docs/TESTING.md](docs/TESTING.md) and [docs/BENCHMARKS.md](docs/BENCHMARKS.md) for exact coverage and benchmark interpretation.

## Security and Deployment Limits

The sidecar has shared-key authentication plus a configured tenant allow-list; it is not a multi-principal identity system. Run it behind TLS termination, a reverse proxy, or a service mesh in production. API keys must be supplied through environment variables or mounted secret files, never committed configuration.

Graph, health, cache, metrics, and circuit-breaker state are in memory. JSONL audit is durable locally, but multiple sidecars do not coordinate routing or health state. Horizontal clustering, distributed state, OTLP collector export, production per-customer identity, and external tool-network validation are **PLANNED**.

Docker files exist, but container validation is `NOT VERIFIED` on a machine without a working Docker runtime.

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Architecture audit](docs/ARCHITECTURE_AUDIT.md)
- [Baseline](docs/BASELINE.md)
- [Phase 1 report](docs/PHASE_1_REPORT.md)
- [Phase 2 report](docs/PHASE_2_REPORT.md)
- [Phase 3 report](docs/PHASE_3_REPORT.md)
- [Testing](docs/TESTING.md)
- [Benchmarks](docs/BENCHMARKS.md)
- [Security](SECURITY.md)
- [Contributing](CONTRIBUTING.md)
- [Phase 4 report](docs/PHASE_4_REPORT.md)

- [Phase 5 report](docs/PHASE_5_REPORT.md)

- [Phases 6-7 report](docs/PHASE_6_7_REPORT.md)
