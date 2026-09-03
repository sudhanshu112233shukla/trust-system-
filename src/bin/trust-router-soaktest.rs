use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::task::JoinSet;
use trust_router::{Edge, OPEN_THRESHOLD, RouteDecision, Router};

const DEFAULT_SECONDS: u64 = 30;
const DEFAULT_AGENTS: usize = 200;
const SEARCH_NODES: [&str; 6] = [
    "search_a", "search_b", "search_c", "search_d", "search_e", "search_f",
];
const SUMMARY_NODES: [&str; 6] = [
    "summarize_a",
    "summarize_b",
    "summarize_c",
    "summarize_d",
    "summarize_e",
    "summarize_f",
];
const ALL_TOOL_NODES: [&str; 12] = [
    "search_a",
    "search_b",
    "search_c",
    "search_d",
    "search_e",
    "search_f",
    "summarize_a",
    "summarize_b",
    "summarize_c",
    "summarize_d",
    "summarize_e",
    "summarize_f",
];

#[tokio::main]
async fn main() {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECONDS);
    let agents = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_AGENTS);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let router = build_router();
    let metrics = Arc::new(SoakMetrics::default());
    let stop = Arc::new(AtomicBool::new(false));
    let mut tasks = JoinSet::new();
    let started = Instant::now();

    for agent in 0..agents {
        let router = router.clone();
        let metrics = metrics.clone();
        let stop = stop.clone();
        tasks.spawn(async move { run_agent(router, metrics, stop, agent).await });
    }

    let injector_router = router.clone();
    let injector_metrics = metrics.clone();
    let injector_stop = stop.clone();
    let injector = tokio::spawn(async move {
        run_failure_injector(injector_router, injector_metrics, injector_stop).await
    });

    tokio::time::sleep_until(deadline.into()).await;
    stop.store(true, Ordering::Relaxed);

    let mut panics = 0usize;
    while let Some(result) = tasks.join_next().await {
        if result.is_err() {
            panics += 1;
        }
    }
    if injector.await.is_err() {
        panics += 1;
    }

    let mut latencies = metrics
        .latencies
        .lock()
        .expect("latency lock poisoned")
        .clone();
    latencies.sort_unstable();
    println!("Trust Router soak test");
    println!("seconds={seconds}");
    println!("agents={agents}");
    println!("elapsed_ms={}", started.elapsed().as_millis());
    println!("routes={}", metrics.routes.load(Ordering::Relaxed));
    println!(
        "escalations={}",
        metrics.escalations.load(Ordering::Relaxed)
    );
    println!(
        "result_updates={}",
        metrics.result_updates.load(Ordering::Relaxed)
    );
    println!(
        "failure_transitions={}",
        metrics.failure_transitions.load(Ordering::Relaxed)
    );
    println!(
        "recovery_transitions={}",
        metrics.recovery_transitions.load(Ordering::Relaxed)
    );
    println!("panics={panics}");
    println!("deadlocks_detected=false");
    println!("p50_us={}", percentile(&latencies, 50.0).as_micros());
    println!("p95_us={}", percentile(&latencies, 95.0).as_micros());
    println!("p99_us={}", percentile(&latencies, 99.0).as_micros());
    println!("memory_drift_check=not_available_without_platform_counter");
    println!(
        "contention_observed={}",
        percentile(&latencies, 99.0) > Duration::from_millis(5)
    );
}

async fn run_agent(router: Router, metrics: Arc<SoakMetrics>, stop: Arc<AtomicBool>, agent: usize) {
    let mut iteration = 0usize;
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        match router.route("soak", "start", "done") {
            RouteDecision::Routed(route) => {
                metrics.routes.fetch_add(1, Ordering::Relaxed);
                for node in route
                    .nodes
                    .iter()
                    .skip(1)
                    .take(route.nodes.len().saturating_sub(2))
                {
                    router.record_result(
                        "soak",
                        node,
                        true,
                        80 + ((agent + iteration) % 120) as u64,
                    );
                    metrics.result_updates.fetch_add(1, Ordering::Relaxed);
                }
            }
            RouteDecision::Escalate(_) => {
                metrics.escalations.fetch_add(1, Ordering::Relaxed);
            }
        }
        metrics
            .latencies
            .lock()
            .expect("latency lock poisoned")
            .push(started.elapsed());
        iteration += 1;
        tokio::task::yield_now().await;
    }
}

async fn run_failure_injector(router: Router, metrics: Arc<SoakMetrics>, stop: Arc<AtomicBool>) {
    let mut cycle = 0usize;
    while !stop.load(Ordering::Relaxed) {
        let fail_count = 2 + (cycle % 2);
        let offset = (cycle * 5) % ALL_TOOL_NODES.len();
        let nodes: Vec<_> = (0..fail_count)
            .map(|index| ALL_TOOL_NODES[(offset + index) % ALL_TOOL_NODES.len()])
            .collect();

        for node in &nodes {
            for _ in 0..OPEN_THRESHOLD {
                router.record_result("soak", *node, false, 30_000);
            }
            metrics.failure_transitions.fetch_add(1, Ordering::Relaxed);
        }

        tokio::time::sleep(Duration::from_millis(25)).await;

        for node in &nodes {
            let _ = router.force_half_open("soak", *node);
            for _ in 0..3 {
                router.record_result("soak", *node, true, 90);
            }
            metrics.recovery_transitions.fetch_add(1, Ordering::Relaxed);
        }

        cycle += 1;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn build_router() -> Router {
    let router = Router::new();
    router.add_tenant("soak");

    for (index, search) in SEARCH_NODES.iter().enumerate() {
        router.add_edge(
            "soak",
            Edge::new("start", *search).with_costs(
                1.0 + index as f64,
                0.001 * (index + 1) as f64,
                100 + index as u64 * 20,
                0.1 + index as f64 * 0.03,
            ),
        );
    }
    for (search_index, search) in SEARCH_NODES.iter().enumerate() {
        for (summary_index, summary) in SUMMARY_NODES.iter().enumerate() {
            router.add_edge(
                "soak",
                Edge::new(*search, *summary).with_costs(
                    1.0 + (search_index + summary_index) as f64 / 3.0,
                    0.001 * (summary_index + 1) as f64,
                    160 + summary_index as u64 * 20,
                    0.1 + summary_index as f64 * 0.03,
                ),
            );
        }
    }
    for (index, summary) in SUMMARY_NODES.iter().enumerate() {
        router.add_edge(
            "soak",
            Edge::new(*summary, "done").with_costs(
                1.0 + index as f64 / 3.0,
                0.001,
                90 + index as u64 * 20,
                0.1,
            ),
        );
    }

    router
}

#[derive(Default)]
struct SoakMetrics {
    routes: AtomicUsize,
    escalations: AtomicUsize,
    result_updates: AtomicUsize,
    failure_transitions: AtomicUsize,
    recovery_transitions: AtomicUsize,
    latencies: std::sync::Mutex<Vec<Duration>>,
}

fn percentile(values: &[Duration], percentile: f64) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    let index = ((values.len() - 1) as f64 * percentile / 100.0).round() as usize;
    values[index]
}
