# Trust Router

Trust Router is a deterministic routing core for agent/tool workflows. It is built to reduce LLM control-plane calls by routing routine work through a cost-weighted graph, then escalating to an LLM only when no feasible path exists.

The current crate includes the production routing core plus a local Axum REST sidecar. It focuses on the parts that must be correct before adding hosted dashboards or deeper customer-specific integrations:

- Multi-tenant tool graphs.
- Tenant-configurable, normalized cost-aware shortest-path routing.
- Circuit-breaker health states: `Healthy`, `Degraded`, `Open`, and `HalfOpen`.
- Health-aware rerouting around open tools while keeping degraded and half-open tools routable with penalties.
- Path caching with node-scoped invalidation on health state transitions.
- Thread-safe route and health updates using a standard-library `RwLock`.
- Explicit audit events for reroutes and escalations.
- Escalation context that can later be passed to an LLM.
- Demo binaries for CLI scenario playback and statistical baseline comparison.
- An Axum REST sidecar for local deployment, SDK verification, metrics, and audit persistence.

Multi-tenancy here means graph and health-state isolation only. It does not yet include authentication, authorization, billing isolation, or network-level tenant security.

## Quick Start

```powershell
cargo test
cargo run --bin trust-router-demo -- demo-audit.jsonl
cargo run --bin trust-router-baseline
cargo run --bin trust-router-loadtest
powershell -ExecutionPolicy Bypass -File .\scripts\verify.ps1
```

## Demo Runbook

Run the deterministic demo:

```powershell
cargo run --bin trust-router-demo -- demo-audit.jsonl
```

The demo shows five states:

1. Normal route: `start -> primary_search -> summarize -> done`
2. Cached route reuse.
3. Primary tool failure: deterministic reroute to `fallback_search`.
4. Compound failure: explicit escalation, never a silent skip.
5. Half-open recovery probe: primary tool becomes cautiously routable again.

It writes fsynced JSONL audit events to `demo-audit.jsonl`.

Run the statistical baseline:

```powershell
cargo run --bin trust-router-baseline
```

This compares a pure ReAct-style control loop against Trust Router across 30 deterministic runs and reports success rate, mean LLM calls, standard deviation, and control-plane LLM-call reduction.

Run the concurrency load test:

```powershell
cargo run --bin trust-router-loadtest
```

By default this spawns 100 concurrent tasks issuing route and health-result updates against the same tenant, then reports p50/p95/p99 decision latency, panic count, and whether a deadlock was detected.

Run the machine-facing sidecar:

```powershell
cargo run --bin trust-router-sidecar -- 127.0.0.1:7878 sidecar-audit.jsonl
```

Example endpoints:

```text
GET  /healthz
GET  /metrics  # requires X-API-Key
GET  /route?tenant=yc-demo&start=start&goal=done  # requires X-API-Key
POST /result?tenant=yc-demo&node=primary_search&success=false&latency_ms=1000  # requires X-API-Key
POST /force-half-open?tenant=yc-demo&node=primary_search  # requires X-API-Key
```

The sidecar persists each route decision audit event to `sidecar-audit.jsonl` before returning the route response.

Sample `/metrics` response after four route decisions, one forced fallback route, and one escalation:

```json
{
  "total_routes": 4,
  "total_reroutes": 3,
  "total_escalations": 1,
  "escalation_rate": 0.25,
  "llm_calls_avoided": 3,
  "false_escalations": 0,
  "false_escalation_rate": 0.0,
  "node_health": {
    "start": "Healthy",
    "primary_search": "Open",
    "fallback_search": "Open",
    "summarize": "Healthy",
    "done": "Healthy"
  },
  "route_latency_us": {
    "p50": 84,
    "p95": 142,
    "p99": 142
  },
  "tenants": {
    "yc-demo": {
      "total_routes": 4,
      "total_reroutes": 3,
      "total_escalations": 1,
      "escalation_rate": 0.25,
      "llm_calls_avoided": 3,
      "false_escalations": 0,
      "false_escalation_rate": 0.0,
      "node_health": {
        "start": "Healthy",
        "primary_search": "Open",
        "fallback_search": "Open",
        "summarize": "Healthy",
        "done": "Healthy"
      },
      "route_latency_us": {
        "p50": 84,
        "p95": 142,
        "p99": 142
      }
    }
  }
}
```

Metrics fields:

- `total_routes`: every `/route` decision since this sidecar process started.
- `total_reroutes`: `/route` calls that found a deterministic graph path.
- `total_escalations`: `/route` calls that had no feasible path and would require LLM recovery.
- `escalation_rate`: `total_escalations / total_routes`.
- `llm_calls_avoided`: route decisions handled without LLM escalation.
- `false_escalations`: escalations where the sidecar observed a penalized node state at decision time; this should stay at zero in the current verified graph because degraded and half-open nodes remain routable.
- `false_escalation_rate`: `false_escalations / total_routes`.
- `node_health`: current sidecar process health state for each demo graph node.
- `route_latency_us`: p50/p95/p99 decision latency in microseconds, measured around the route decision path.
- `tenants`: the same counters broken down per tenant.


## Sidecar Authentication

The local sidecar uses single shared API-key authentication for protected endpoints. Set `TRUST_ROUTER_API_KEY` before starting the sidecar; if omitted, the demo default is `trust-router-demo-key`.

Protected endpoints require `X-API-Key`: `/route`, `/result`, `/force-half-open`, and `/metrics`. `/healthz` stays open for health checks.

This is shared-key sidecar auth, not per-tenant credentials or authorization. In production, run the sidecar behind TLS termination and a reverse proxy or service mesh that owns network policy, secret distribution, and stronger identity controls.
## Running As A Local Sidecar Container

Build the sidecar image locally:

```powershell
docker build -t trust-router-sidecar:local .
```

Run the sidecar container with a host-mounted audit directory:

```powershell
New-Item -ItemType Directory -Force -Path docker-data\audit
docker run --rm -p 7878:7878 -v ${PWD}\docker-data\audit:/audit trust-router-sidecar:local
```

Health check:

```powershell
Invoke-RestMethod -Uri http://127.0.0.1:7878/healthz
```

Run the local pilot simulation with Docker Compose:

```powershell
docker compose up --build --abort-on-container-exit toy-agent
```

The compose stack has two services:

- `trust-router-sidecar`: runs the Rust sidecar and writes audit events to `/audit/sidecar-audit.jsonl`.
- `toy-agent`: a separate Python container that calls the sidecar over the compose network at `http://trust-router-sidecar:7878`.

Run the persistence check:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\docker-persistence-check.ps1
```

This starts the sidecar, runs the toy agent, verifies `docker-data\audit\sidecar-audit.jsonl` exists, restarts only the sidecar container, and confirms `/healthz` comes back up.

State persistence:

- Persisted: append-only audit JSONL under `docker-data\audit`.
- Not persisted yet: in-memory graph state, health state, route cache, and circuit-breaker state. On restart, the demo graph is recreated by the sidecar binary and health/cache state starts fresh.

Run an HTTP load test against the containerized sidecar:

```powershell
python scripts/container_loadtest.py
```

## Deploying To Render

This repository includes render.yaml for a Docker-based Render Web Service.

1. In Render, create a Blueprint from this GitHub repository.
2. Set TRUST_ROUTER_API_KEY to a long random secret in the Render dashboard. Do not use the checked-in demo key.
3. Deploy. Render supplies PORT automatically; the sidecar binds to 0.0.0.0 on that port.
4. Confirm GET /healthz returns {"ok":true}. All other endpoints require X-API-Key.

The Render Free plan is suitable for demonstrations only. It can sleep when idle and local audit JSONL files are ephemeral, so audit history and in-memory health state are not durable across a restart. Use managed storage and a paid always-on service before relying on it for customer workloads.

## Python SDK Stub

The minimal client in `sdk/python/trust_router_client.py` wraps the sidecar's `/route` and `/result` endpoints with `requests`.

```python
from trust_router_client import TrustRouterClient

router = TrustRouterClient(base_url="http://127.0.0.1:7878", tenant="yc-demo")

# Conceptual LangChain-style interceptor:
# ask Trust Router for deterministic tool order before spending LLM tokens
# on routine control-flow decisions.
decision = router.route(start="start", goal="done")
if decision["decision"] == "escalate":
    # Call the LLM recovery planner only here.
    pass
else:
    for tool_name in decision["path"][1:]:
        try:
            # tool_registry[tool_name].invoke(...)
            router.report_result(tool_name, success=True, latency_ms=120)
        except Exception:
            router.report_result(tool_name, success=False, latency_ms=30_000)
            raise
```

`sdk/python/langchain_integration_example.py` is a live sidecar integration that sits one level above the raw SDK. LangChain is not installed on this machine, so it uses a minimal hand-rolled `Tool` stand-in with the same basic shape needed for this demo: named callable tools. The script delegates tool order to Trust Router, intentionally fails `primary_search`, reports the failure to the sidecar, reroutes to `fallback_search`, then completes `summarize` without any LLM control-flow call.

Run it against a live sidecar:

```powershell
$env:TRUST_ROUTER_URL = "http://127.0.0.1:7878"
python sdk/python/langchain_integration_example.py
```
Run the live escalation example:

```powershell
$env:TRUST_ROUTER_URL = "http://127.0.0.1:7878"
$env:TRUST_ROUTER_API_KEY = "trust-router-demo-key"
python sdk/python/escalation_example.py
```

This intentionally opens both search paths and shows the SDK caller the explicit `{"decision":"escalate"}` response rather than a silent failure.


## Optional Python Provider Integrations

`sdk/python/integrations.py` adds three optional integration helpers on top of the raw SDK:

- `RoutedToolExecutor`: routes a named tool registry through Trust Router before execution.
- `OpenAIEscalationAdapter`: uses the official `openai` Python SDK only after Trust Router returns an explicit escalation.
- `McpToolInterceptor`: wraps an MCP SDK `ClientSession.call_tool` flow so MCP tool execution reports success/failure back to Trust Router.

The local verifier imports these adapters successfully. A real OpenAI recovery call still requires `OPENAI_API_KEY` and a selected model. LangChain itself is not installed on this machine, so the checked live tool-loop example remains the hand-rolled LangChain-style stand-in rather than a real LangChain package demo.

## TypeScript SDK Stub

The TypeScript client in `sdk/typescript/trustRouterClient.ts` mirrors the Python SDK with `route(start, goal)` and `reportResult(node, success, latencyMs)` methods over the sidecar REST API.

Run the live sidecar verification script:

```powershell
$env:TRUST_ROUTER_URL = "http://127.0.0.1:7878"
npm run tsc
node --experimental-strip-types sdk/typescript/exampleUsage.ts
```

This script asks the sidecar for the primary path, reports enough `primary_search` failures to open that node, then confirms the next route deterministically uses `fallback_search`.


## Production Readiness

Implemented and tested:

- Deterministic routing core with explicit escalation.
- Circuit-breaker health states and automatic half-open recovery.
- Durable JSONL audit records.
- Axum sidecar API with metrics, per-tenant metric breakdowns, graceful Ctrl+C shutdown, and shared API-key protection.
- Rust unit tests, integration tests, HTTP chaos tests, load tests, and live SDK checks.

Explicitly not done yet:

- Per-tenant customer credentials and authorization beyond the shared key.
- TLS termination inside the sidecar.
- Horizontal scaling, clustering, or distributed route cache.
- Persistent graph/health/cache state across process restarts.
- Real external network conditions beyond the local mock chaos server.
- Full OpenTelemetry export, cargo-fuzz harness, replay simulation mode, and long soak reports.

## How To Verify Everything Yourself

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\verify.ps1
cargo run --bin trust-router-loadtest
cargo run --bin trust-router-demo -- demo-audit.jsonl
cargo run --bin trust-router-baseline
```

With a sidecar running:

```powershell
cargo run --bin trust-router-sidecar -- 127.0.0.1:7878 sidecar-audit.jsonl
$env:TRUST_ROUTER_API_KEY = "trust-router-demo-key"
python sdk/python/example_usage.py
python sdk/python/langchain_integration_example.py
python sdk/python/escalation_example.py
npm run tsc
node --experimental-strip-types sdk/typescript/exampleUsage.ts
```

Docker, fuzzing, and replay commands should be run only after their required local tooling exists.

## Core Model

A tenant owns an isolated graph. Each edge represents a possible tool transition:

- `base_cost`: normalized operational cost.
- `dollar_cost`: expected direct model/API spend.
- `latency_ms`: expected latency.
- `risk`: business or reliability risk.
- `enabled`: static allow/deny control.

Each tenant has a `TenantCostModel`:

- `CostWeights`: how much the tenant cares about dollar cost, latency, risk, and base cost.
- `CostCeilings`: observed tenant-specific ceilings used to normalize raw units into a comparable 0.0-1.0 range.

Each node has a `HealthTracker`:

- `success_rate`
- `p99_latency_ms`
- `consecutive_failures`
- `consecutive_successes`
- `state`

The router combines normalized edge weight and health penalty. `Degraded` and `HalfOpen` nodes remain routable with extra penalty. `Open` nodes are excluded, which forces deterministic rerouting. If no path remains, the router emits an explicit escalation event.

## Example

```rust
use trust_router::{Edge, RouteDecision, Router};

let router = Router::new();
router.add_tenant("acme");
router.add_edge("acme", Edge::new("start", "search").with_costs(1.0, 0.001, 100, 0.1));
router.add_edge("acme", Edge::new("search", "done").with_costs(1.0, 0.001, 100, 0.1));

let decision = router.route("acme", "start", "done");
assert!(matches!(decision, RouteDecision::Routed(_)));

router.record_result("acme", "search", false, 30_000);
```

## Tested Scenarios

The unit tests currently cover:

- Cheapest valid path selection.
- Normalized cost calculation with tenant weights and ceilings.
- Degraded and half-open circuit-breaker behavior.
- Rerouting around a failed/rate-limited tool.
- Escalation when no feasible path exists.
- Cache hits and node-scoped cache invalidation after health state changes.
- Tenant health isolation.
- Audit events for routing and escalation.
- Generated graph invariant checks that every failure ends in a logged reroute or explicit escalation.
- Concurrent route and health-update stress coverage.
- JSONL audit export with durable file sync.

Run:

```powershell
cargo test
```

## Roadmap

1. Add async health monitors that consume real tool/API results and call `record_result`.
2. Replace the demo sidecar with a production REST or gRPC service once dependency fetching is enabled.
3. Add Python and TypeScript SDK interceptors for LangChain, native OpenAI tool calls, and MCP servers.
4. Add OpenTelemetry export after the current append-only JSONL audit path.
5. Add broader chaos-test harnesses for degraded tools, compound failures, and concurrent multi-agent traffic.
6. Add LLM escalation adapter that receives `EscalationContext` and returns a bounded recovery plan.
7. Add dashboard metrics: LLM calls avoided, reroute rate, false escalation rate, p95/p99 routing latency, and per-tenant health.
8. Replace the current coarse `RwLock` with finer-grained tenant locks or `DashMap` once contention appears in load tests.
9. Add real property-based tests with `proptest` once dependency fetching is enabled in CI.

## Known Limitations

- The current `Arc<RwLock>` design uses one coarse router-state lock, so concurrent traffic is safe but not per-node or per-tenant fine-grained yet. If `trust-router-loadtest` shows contention under realistic workloads, the next optimization target is splitting locks by tenant and then by graph, health state, and path cache.
- The sidecar is now a real Axum REST service, but it still uses single shared-key auth and in-memory graph/health/cache state. Per-customer auth, persistent state, TLS termination, and horizontal scaling remain outside this demo build.

## Real Scenario Test Plan

- Baseline comparison: run the same workflow through a pure LLM-controlled agent and through Trust Router. Compare task success, control-plane LLM calls, latency, and spend.
- Fault injection: simulate timeout, HTTP 500, and rate-limit states for individual tools. Verify deterministic rerouting.
- Compound failure: degrade multiple tools in one workflow. Verify explicit escalation instead of silent skipping.
- Load test: route many concurrent workflows and measure decision latency.
- False escalation test: verify the router does not escalate when a valid degraded-but-available path exists.
- Statistical baseline: run each task 20-30 times per arm and report mean/stddev for success rate and LLM-call count.






