# Benchmarks

## Planner Benchmark

```powershell
cargo run --release --bin trust-router-planner-benchmark -- 20000
```

Interpretation:

- `router_forced_cache_miss`: baseline deterministic route calculation with cache keys intentionally cycled beyond the LRU capacity.
- `router_core_cache_hit`: cached router decision.
- `planner_forced_cache_miss`: request validation plus deterministic execution-plan construction when the underlying core cannot reuse a cache entry.
- `planner_core_cache_hit`: planner overhead when the router provides a cached route.

`backend_inference_calls=0` is intentional. This benchmark isolates planning from a model or tool backend. It does not report memory or CPU; those appear as `NOT_MEASURED` until a platform-specific profiler is added.

## Existing Load and Soak

`trust-router-loadtest` exercises concurrent routing and health updates. `trust-router-soaktest` adds repeated multi-agent health failures/recoveries across a denser graph. Review p50/p95/p99, throughput, escalation count, panics, deadlocks, and `contention_observed`; do not replace the coarse lock without measured contention.