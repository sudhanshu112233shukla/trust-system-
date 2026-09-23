# Changelog

## 0.1.0 - Initial Hardened Core

- Deterministic tenant-isolated graph routing with normalized cost, circuit breakers, bounded cache, audit JSONL, recovery probes, and explicit escalation.
- Axum sidecar, shared-key authentication, SDK examples, metrics, OpenTelemetry stdout export, load/soak/chaos/durability coverage, and Docker packaging.

## Unreleased - Phase 0-3 Planner Hardening

- Added validated sidecar configuration with development/production mode, secret-file support, cost and health policy validation, tenant allow-list, and rate-limit configuration.
- Added bounded rolling p99 latency samples, bounded audit history, and corrected route/cache/reroute metrics.
- Added deterministic `ExecutionPlan` and bounded recovery planner with sidecar `/plan` endpoint and planner performance benchmark.
- Added HTTP security controls: tenant authorization, token-bucket limiting, structured error responses, request IDs, security header, and POST-only mutations.
- Added Phase 0-3 architecture, audit, baseline, test, and benchmark documentation.