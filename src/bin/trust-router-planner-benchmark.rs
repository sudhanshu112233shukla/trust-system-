use std::time::{Duration, Instant};

use trust_router::planner::{Planner, PlanningRequest};
use trust_router::{Edge, Router};

const DEFAULT_ITERATIONS: usize = 20_000;
const UNIQUE_ROUTES: usize = 2_048;

fn main() {
    let iterations = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .filter(|value: &usize| *value > 0)
        .unwrap_or(DEFAULT_ITERATIONS);

    println!("Trust Router planner benchmark");
    println!("iterations={iterations}");
    println!("unique_routes={UNIQUE_ROUTES}");
    println!("backend_inference_calls=0");

    let router_miss = benchmark_router(iterations, false);
    print_result("router_forced_cache_miss", router_miss);
    let router_hit = benchmark_router(iterations, true);
    print_result("router_core_cache_hit", router_hit);
    let planner_miss = benchmark_planner(iterations, false);
    print_result("planner_forced_cache_miss", planner_miss);
    let planner_hit = benchmark_planner(iterations, true);
    print_result("planner_core_cache_hit", planner_hit);

    println!("memory_bytes=NOT_MEASURED (portable benchmark has no allocator profiler)");
    println!("cpu_percent=NOT_MEASURED (portable benchmark has no process sampler)");
}

fn benchmark_router(iterations: usize, cache_hit: bool) -> BenchmarkResult {
    let router = benchmark_router_graph();
    measure(iterations, |index| {
        let route_id = if cache_hit { 0 } else { index % UNIQUE_ROUTES };
        let _ = router.route(
            "benchmark",
            format!("start_{route_id}"),
            format!("done_{route_id}"),
        );
    })
}

fn benchmark_planner(iterations: usize, cache_hit: bool) -> BenchmarkResult {
    let planner = Planner::new(benchmark_router_graph());
    measure(iterations, |index| {
        let route_id = if cache_hit { 0 } else { index % UNIQUE_ROUTES };
        let _ = planner
            .plan(PlanningRequest::new(
                "benchmark",
                format!("start_{route_id}"),
                format!("done_{route_id}"),
            ))
            .expect("benchmark graph must be feasible");
    })
}

fn benchmark_router_graph() -> Router {
    let router = Router::new();
    router.add_tenant("benchmark");
    for index in 0..UNIQUE_ROUTES {
        router.add_edge(
            "benchmark",
            Edge::new(format!("start_{index}"), format!("tool_{index}"))
                .with_costs(1.0, 0.001, 20, 0.1),
        );
        router.add_edge(
            "benchmark",
            Edge::new(format!("tool_{index}"), format!("done_{index}"))
                .with_costs(1.0, 0.001, 20, 0.1),
        );
    }
    router
}

fn measure(iterations: usize, mut operation: impl FnMut(usize)) -> BenchmarkResult {
    let mut samples = Vec::with_capacity(iterations);
    let started = Instant::now();
    for index in 0..iterations {
        let before = Instant::now();
        operation(index);
        samples.push(before.elapsed());
    }
    let elapsed = started.elapsed();
    samples.sort_unstable();
    BenchmarkResult {
        iterations,
        elapsed,
        p50: percentile(&samples, 50.0),
        p95: percentile(&samples, 95.0),
        p99: percentile(&samples, 99.0),
    }
}

fn percentile(values: &[Duration], percentile: f64) -> Duration {
    let index = ((values.len() - 1) as f64 * percentile / 100.0).round() as usize;
    values[index]
}

struct BenchmarkResult {
    iterations: usize,
    elapsed: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
}

fn print_result(name: &str, result: BenchmarkResult) {
    let throughput = result.iterations as f64 / result.elapsed.as_secs_f64();
    println!(
        "{name} p50_us={} p95_us={} p99_us={} throughput_ops_per_sec={throughput:.0}",
        result.p50.as_micros(),
        result.p95.as_micros(),
        result.p99.as_micros(),
    );
}
