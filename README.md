# Trust Router

**Deterministic control-plane decisions for AI-agent and inference workflows.**

Trust Router replaces routine LLM control-plane decisions with a cost-aware, health-aware deterministic planner. It selects a safe path, records the decision durably, and returns an explicit bounded recovery outcome when no feasible path exists.

> The project optimizes only with declared capabilities and measured inputs. Unknown KV compatibility, quality, latency, or reliability data never becomes an invented benefit.

## Why It Exists

AI-agent workflows often use an LLM to choose ordinary next steps even when graph, cost, health, and policy information already determine a safe choice. Trust Router moves those repeatable decisions into a small, testable Rust control plane.

```text
request
  -> validation + tenant authorization
  -> deterministic routing / inference decision
  -> bounded ExecutionPlan or bounded recovery
  -> execution adapter reports outcome
  -> health, cache, audit, and metrics update
```

## Capabilities

| Area | Implemented behavior |
| --- | --- |
| Routing | Tenant-isolated shortest paths with normalized cost, disabled-edge filtering, and deterministic selection. |
| Reliability | Healthy, Degraded, Open, and HalfOpen circuit states with automatic recovery probes. |
| Caching | Bounded per-tenant LRU route cache with node-scoped invalidation. |
| Planning | Validated, bounded `ExecutionPlan` or explicit bounded recovery plan. |
| KV intelligence | Version-aware model registry, declared KV capabilities, measurement-backed compatibility checks, and safe normal-prefill fallback. |
| Inference decisions | Constraint-first filtering, explicit rejection reasons, transparent scoring, deterministic ties, and bounded fallback order. |
| Execution boundary | Generic `InferenceBackend` trait plus a clearly test-only deterministic mock backend. |
| Security | API-key auth, tenant allow-list authorization, constant-time comparison, limits, request IDs, structured errors, and rate limiting. |
| Audit and telemetry | Durable JSONL decisions, bounded metrics, and optional stdout OpenTelemetry metrics. |

## Architecture

```text
Application / SDK
      |
      v
Axum sidecar  -- auth | authorization | validation | rate limiting
      |
      v
Planner + inference decision engine
      |-- router graph, health, and bounded route cache
      |-- model / KV capability and compatibility registries
      |-- constraint filter, deterministic scoring, bounded fallbacks
      |
      +--> ExecutionPlan --> adapter / backend
      +--> bounded recovery --> operator or explicit LLM escalation
      |
      v
JSONL audit + metrics + health-result ingestion
```

The planner does **not** execute tools, call an LLM, transfer KV state, schedule GPUs, or make network calls. Those remain adapter/backend responsibilities.

## Quick Start

```powershell
cargo test
cargo run --bin trust-router-demo -- demo-audit.jsonl
cargo run --bin trust-router-sidecar -- 127.0.0.1:7878 sidecar-audit.jsonl
```

Local development uses `X-API-Key: trust-router-demo-key`. Set `TRUST_ROUTER_API_KEY` for a different local key.

```powershell
curl -H "X-API-Key: trust-router-demo-key" "http://127.0.0.1:7878/plan?tenant=yc-demo&start=start&goal=done"
```

## API

| Endpoint | Purpose |
| --- | --- |
| `GET /healthz` | Unauthenticated process health check. |
| `GET /route` | Raw deterministic route decision. |
| `GET /plan` | Bounded execution or recovery plan. |
| `POST /result` | Report tool/backend outcome for health tracking. |
| `POST /force-half-open` | Manually trigger a guarded recovery probe. |
| `GET /metrics` | Bounded route and planner operational metrics. |

All endpoints except `/healthz` require `X-API-Key`. Tenant endpoints also require a configured allow-list match.

A successful plan retains the stable Phase 1 plan shape:

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

Errors have a stable non-secret response shape:

```json
{"error":{"code":"tenant_not_authorized","message":"API key is not authorized for this tenant","request_id":"tr-0000000000000001"}}
```

## KV and Inference Safety

Cross-model KV transfer is selected only when all of the following are registered and sufficient: source/target versions, declared capability, mapping version, positive sample count, latency measurements, quality retention, confidence, and failure rate. Otherwise the planner records a structured fallback reason and selects normal prefill.

The Phase 5 decision engine filters invalid, over-budget, over-latency, and under-quality candidates before it scores them. A strategy is never selected merely because it is cheaper.

## Configuration and Security

Copy [sidecar.config.example.json](sidecar.config.example.json), set `TRUST_ROUTER_CONFIG`, and override operational values with `TRUST_ROUTER_*` environment variables.

Production startup rejects missing/demo/short API keys, malformed policy values, and an absent tenant allow-list. Use environment variables or mounted secret files for secrets. Deploy behind TLS termination, a reverse proxy, or a service mesh.

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

Historical local Phase 3 planner measurements were cache-hit p99 `14us` and forced-miss p99 `53us`; rerun the benchmark on your own hardware before making performance claims.

## Current Limits

- Graph, health, cache, metrics, model registry, and KV registry state are process-local and reset on restart.
- Audit JSONL is durable locally; clustered sidecars do not coordinate state.
- Docker files exist but container validation remains **NOT VERIFIED** without a working Docker daemon.
- The generic mock backend is simulation only. No production inference backend, distributed KV store, GPU scheduler, real cross-model KV transfer, or OneTriangle integration exists.

## Documentation

- [Architecture](docs/ARCHITECTURE.md) and [architecture audit](docs/ARCHITECTURE_AUDIT.md)
- [Testing](docs/TESTING.md), [benchmarks](docs/BENCHMARKS.md), and [baseline](docs/BASELINE.md)
- [Phase 1](docs/PHASE_1_REPORT.md), [Phase 2](docs/PHASE_2_REPORT.md), [Phase 3](docs/PHASE_3_REPORT.md), [Phase 4](docs/PHASE_4_REPORT.md), [Phase 5](docs/PHASE_5_REPORT.md), and [Phases 6-7](docs/PHASE_6_7_REPORT.md)
- [Security](SECURITY.md) and [contributing](CONTRIBUTING.md)