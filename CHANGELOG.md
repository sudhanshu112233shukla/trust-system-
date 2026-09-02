# Changelog

## 0.1.0 - Initial Hardened Core

- Added deterministic multi-tenant graph routing for agent/tool workflows.
- Added normalized cost model covering base cost, dollar cost, latency, and risk.
- Added circuit-breaker health states: `Healthy`, `Degraded`, `Open`, and `HalfOpen`.
- Added node-scoped path cache invalidation.
- Added automatic open-to-half-open recovery loop.
- Added durable JSONL audit export.
- Added sidecar HTTP API with health, route, result, recovery, and metrics endpoints.
- Added shared API-key protection for non-health endpoints.
- Added Python and TypeScript SDK stubs with live sidecar verification.
- Added LangChain-style tool-loop demonstration using a local stand-in.
- Added chaos, durability, metrics, auth, and concurrent load coverage.
- Added Docker packaging files for local sidecar deployment.
