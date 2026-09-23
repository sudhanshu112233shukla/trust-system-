# Architecture Audit

## Current findings

- The core has a single `Arc<RwLock<RouterState>>`. This is correct under concurrent routing and health updates, but unrelated tenants can contend. Existing load and short soak runs did not show contention, so a lock redesign is **PLANNED**, not implemented.
- Router cache is bounded to 1,024 entries per tenant and maintained in LRU order. Relevant graph, health, and cost changes invalidate it.
- In-memory audit events are bounded to 10,000. Durable JSONL writes are appended and `sync_all()` is called before a sidecar response.
- Node latency uses the latest 128 samples to compute a bounded rolling p99 approximation.
- The planner has no independent path cache. It intentionally relies on the router cache so health transitions cannot leave a stale execution plan cache behind.
- The sidecar validates identifiers, requires a shared key, authorizes configured tenant IDs, compares keys in constant time, and token-bucket limits protected requests.

## Deferred risks

TLS, external identity providers, per-principal authorization, distributed state, audit tamper evidence, cache sharing, OTLP collector export, full allocator/CPU profiling, and multi-instance consistency are not implemented.