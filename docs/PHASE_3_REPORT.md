# Phase 3 Report: Planner Performance Validation

The `trust-router-planner-benchmark` binary measures routing and planning separately with no backend inference calls. It reports p50/p95/p99 and throughput for two cases:

- forced cache misses by cycling across more route keys than the bounded cache holds;
- core cache hits by reusing one route key.

Verified locally on 2026-09-23 with 5,000 iterations:

```text
router_forced_cache_miss  p50=14us  p95=19us  p99=46us  throughput=53,193 ops/s
router_core_cache_hit     p50=2us   p95=4us   p99=7us   throughput=278,604 ops/s
planner_forced_cache_miss p50=16us  p95=21us  p99=53us  throughput=46,631 ops/s
planner_core_cache_hit    p50=4us   p95=5us   p99=14us  throughput=169,320 ops/s
backend_inference_calls=0
```

The benchmark confirms that planner construction adds small, measurable overhead above direct routing on this machine. It does not report memory or CPU; both remain `NOT_MEASURED` until a platform-specific profiler or allocator profiler is added. Numbers must be remeasured on deployment hardware.

Run:

```powershell
cargo run --release --bin trust-router-planner-benchmark -- 20000
```

The planner adds no external inference calls and no independent stale-plan cache.