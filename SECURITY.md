# Security

## Covered Today

- Non-health sidecar endpoints require an `X-API-Key` header.
- API-key comparison uses constant-time byte comparison.
- Audit JSONL output escapes control characters so route data does not corrupt the log format.
- The chaos and durability tests verify explicit failure handling and parseable audit output.

## Not Covered Yet

- No TLS termination is built into the sidecar. Run it behind a reverse proxy or service mesh that provides TLS in real deployments.
- API-key auth is a single shared key, not per-tenant credentials or user-level authorization.
- Graph, health, cache, and circuit-breaker state are in-memory and reset on process restart.
- There is no horizontal clustering or distributed cache coordination yet.
- Per-key rate limiting and stricter sidecar input length limits are still future hardening items.

## Reporting Vulnerabilities

For now, report security issues privately to the project owner before opening public issues. Include reproduction steps, affected endpoint or SDK, expected behavior, and observed behavior.
