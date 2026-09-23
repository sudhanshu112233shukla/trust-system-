# Security

## Implemented Controls

- `/healthz` is intentionally public for liveness checks. Other sidecar endpoints require `X-API-Key`.
- API-key comparison performs a fixed-length traversal to avoid early-exit equality behavior.
- The configured tenant allow-list is enforced for `/route`, `/plan`, `/result`, and `/force-half-open`. A key cannot select an unlisted tenant.
- Identifiers are limited to 1-128 ASCII alphanumeric, `_`, `-`, or `.` characters. This rejects control characters and prevents JSONL log injection through tenant or node names.
- Protected endpoints use a bounded token-bucket limiter configured at startup. A limit returns `429` with `Retry-After`.
- Error responses have a stable schema and do not expose internal filesystem or persistence errors. Protected responses include a request ID and `X-Content-Type-Options: nosniff`.
- Production startup requires an explicit non-demo API key of at least 32 printable characters, supplied through `TRUST_ROUTER_API_KEY` or `TRUST_ROUTER_API_KEY_FILE`.
- Audit events are serialized with `serde_json`, appended as JSONL, and synced before a sidecar route or plan response is sent. API keys and headers are never included in audit events or telemetry attributes.

## Operational Requirements

Terminate TLS outside the sidecar with a reverse proxy, service mesh, or platform ingress. The sidecar does not implement TLS itself. Keep production secrets out of config files committed to Git; prefer environment injection or a mounted secret file.

## Residual Risks and Planned Work

- Shared-key authentication plus tenant allow-list is not per-user or per-customer identity.
- Health, graph, cache, metrics, and circuit-breaker state are in-memory; a restart clears them.
- Multi-instance coordination, audit tamper evidence, OTLP collector export, and externally hosted security scanning are **PLANNED**.
- Docker deployment is `NOT VERIFIED` on machines without a working Docker runtime.

## Reporting a Vulnerability

Report vulnerabilities privately to the project owner with a minimal reproduction, affected version or endpoint, expected behavior, observed behavior, and any logs with secrets removed.