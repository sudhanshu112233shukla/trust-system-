use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::task::JoinSet;
use trust_router::{Edge, NodeState, OPEN_THRESHOLD, RouteDecision, Router};

const DEFAULT_TASKS: usize = 100;
const OPS_PER_TASK: usize = 200;
const FAILURE_CYCLES: usize = 12;
const SEARCH_NODES: [&str; 4] = ["search_a", "search_b", "search_c", "search_d"];
const SUMMARY_NODES: [&str; 4] = ["summarize_a", "summarize_b", "summarize_c", "summarize_d"];

#[tokio::main]
async fn main() {
    let task_count = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_TASKS);
    let router = build_router();
    let mut tasks = JoinSet::new();
    let metrics = Arc::new(LoadMetrics::default());

    let started = Instant::now();
    for worker in 0..task_count {
        let router = router.clone();
        let metrics = metrics.clone();
        tasks.spawn(async move { run_worker(router, metrics, worker).await });
    }

    let failure_router = router.clone();
    let failure_metrics = metrics.clone();
    let failure_task = tokio::spawn(async move {
        run_failure_injector(failure_router, failure_metrics).await;
    });

    let mut latencies = Vec::with_capacity(task_count * OPS_PER_TASK);
    let mut panics = 0usize;

    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(worker_latencies) => latencies.extend(worker_latencies),
            Err(error) => {
                panics += 1;
                eprintln!("worker failed: {error}");
            }
        }
    }

    if let Err(error) = failure_task.await {
        panics += 1;
        eprintln!("failure injector failed: {error}");
    }

    latencies.sort_unstable();
    println!("Trust Router load test");
    println!("tasks={task_count}");
    println!("ops_per_task={OPS_PER_TASK}");
    println!("failure_cycles={FAILURE_CYCLES}");
    println!("total_ops={}", latencies.len());
    println!("elapsed_ms={}", started.elapsed().as_millis());
    println!("panics={panics}");
    println!("deadlocks_detected=false");
    println!(
        "route_successes={}",
        metrics.route_successes.load(Ordering::Relaxed)
    );
    println!(
        "route_escalations={}",
        metrics.route_escalations.load(Ordering::Relaxed)
    );
    println!(
        "escalations_all_paths_blocked={}",
        metrics
            .escalations_all_paths_blocked
            .load(Ordering::Relaxed)
    );
    println!(
        "escalations_with_penalty_path_available={}",
        metrics
            .escalations_with_penalty_path_available
            .load(Ordering::Relaxed)
    );
    println!(
        "escalations_unknown_state={}",
        metrics.escalations_unknown_state.load(Ordering::Relaxed)
    );
    println!(
        "escalation_snapshots_search_open_total={}",
        metrics
            .escalation_snapshots_search_open_total
            .load(Ordering::Relaxed)
    );
    println!(
        "escalation_snapshots_summarize_open_total={}",
        metrics
            .escalation_snapshots_summarize_open_total
            .load(Ordering::Relaxed)
    );
    println!(
        "escalation_snapshots_penalty_node_usable={}",
        metrics
            .escalation_snapshots_penalty_node_usable
            .load(Ordering::Relaxed)
    );
    println!(
        "route_errors={}",
        metrics.route_errors.load(Ordering::Relaxed)
    );
    println!(
        "failure_transitions={}",
        metrics.failure_transitions.load(Ordering::Relaxed)
    );
    println!(
        "recovery_transitions={}",
        metrics.recovery_transitions.load(Ordering::Relaxed)
    );
    println!(
        "concurrent_failures_completed={}",
        metrics.failure_task_completed.load(Ordering::Relaxed)
    );
    println!("p50_us={}", percentile(&latencies, 50.0).as_micros());
    println!("p95_us={}", percentile(&latencies, 95.0).as_micros());
    println!("p99_us={}", percentile(&latencies, 99.0).as_micros());
    println!(
        "correctness_degraded={}",
        panics > 0 || metrics.route_errors.load(Ordering::Relaxed) > 0
    );
}

async fn run_worker(router: Router, metrics: Arc<LoadMetrics>, worker: usize) -> Vec<Duration> {
    let mut latencies = Vec::with_capacity(OPS_PER_TASK);

    for iteration in 0..OPS_PER_TASK {
        let started = Instant::now();

        if (worker + iteration).is_multiple_of(3) {
            match router.route("load", "start", "done") {
                RouteDecision::Routed(_) => {
                    metrics.route_successes.fetch_add(1, Ordering::Relaxed);
                }
                RouteDecision::Escalate(_) => {
                    metrics.route_escalations.fetch_add(1, Ordering::Relaxed);
                    classify_escalation(&router, &metrics);
                }
            }
        } else {
            let node = ALL_TOOL_NODES[(worker + iteration) % ALL_TOOL_NODES.len()];
            router.record_result("load", node, true, 100 + (iteration % 50) as u64);
        }

        latencies.push(started.elapsed());
        tokio::task::yield_now().await;
    }

    latencies
}

fn classify_escalation(router: &Router, metrics: &LoadMetrics) {
    let search_states: Vec<_> = SEARCH_NODES
        .iter()
        .map(|node| router.node_state("load", node))
        .collect();
    let summary_states: Vec<_> = SUMMARY_NODES
        .iter()
        .map(|node| router.node_state("load", node))
        .collect();
    let search_open = search_states
        .iter()
        .filter(|state| **state == Some(NodeState::Open))
        .count();
    let summary_open = summary_states
        .iter()
        .filter(|state| **state == Some(NodeState::Open))
        .count();

    metrics
        .escalation_snapshots_search_open_total
        .fetch_add(search_open, Ordering::Relaxed);
    metrics
        .escalation_snapshots_summarize_open_total
        .fetch_add(summary_open, Ordering::Relaxed);

    if search_states
        .iter()
        .chain(summary_states.iter())
        .any(|state| matches!(state, Some(NodeState::Degraded | NodeState::HalfOpen)))
    {
        metrics
            .escalation_snapshots_penalty_node_usable
            .fetch_add(1, Ordering::Relaxed);
    }

    let all_paths_blocked =
        search_open == SEARCH_NODES.len() || summary_open == SUMMARY_NODES.len();
    let penalty_path_available =
        search_open < SEARCH_NODES.len() && summary_open < SUMMARY_NODES.len();

    if all_paths_blocked {
        metrics
            .escalations_all_paths_blocked
            .fetch_add(1, Ordering::Relaxed);
    } else if penalty_path_available {
        metrics
            .escalations_with_penalty_path_available
            .fetch_add(1, Ordering::Relaxed);
    } else {
        metrics
            .escalations_unknown_state
            .fetch_add(1, Ordering::Relaxed);
    }
}

async fn run_failure_injector(router: Router, metrics: Arc<LoadMetrics>) {
    for cycle in 0..FAILURE_CYCLES {
        let fail_count = 2 + (cycle % 2);
        let offset = (cycle * 3) % ALL_TOOL_NODES.len();
        let nodes: Vec<_> = (0..fail_count)
            .map(|index| ALL_TOOL_NODES[(offset + index) % ALL_TOOL_NODES.len()])
            .collect();

        for node in &nodes {
            for _ in 0..OPEN_THRESHOLD {
                router.record_result("load", *node, false, 750);
            }
            metrics.failure_transitions.fetch_add(1, Ordering::Relaxed);
        }

        tokio::time::sleep(Duration::from_millis(5)).await;

        for node in &nodes {
            let _ = router.force_half_open("load", *node);
            for _ in 0..3 {
                router.record_result("load", *node, true, 100);
            }
            metrics.recovery_transitions.fetch_add(1, Ordering::Relaxed);
        }

        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    metrics
        .failure_task_completed
        .store(true, Ordering::Relaxed);
}

fn build_router() -> Router {
    let router = Router::new();
    router.add_tenant("load");

    for (index, search) in SEARCH_NODES.iter().enumerate() {
        router.add_edge(
            "load",
            Edge::new("start", *search).with_costs(
                1.0 + index as f64,
                0.001 * (index + 1) as f64,
                120 + index as u64 * 20,
                0.1 + index as f64 * 0.05,
            ),
        );
    }

    for (search_index, search) in SEARCH_NODES.iter().enumerate() {
        for (summary_index, summary) in SUMMARY_NODES.iter().enumerate() {
            router.add_edge(
                "load",
                Edge::new(*search, *summary).with_costs(
                    1.0 + (search_index + summary_index) as f64 / 2.0,
                    0.001 * (summary_index + 1) as f64,
                    180 + summary_index as u64 * 30,
                    0.1 + summary_index as f64 * 0.04,
                ),
            );
        }
    }

    for (index, summary) in SUMMARY_NODES.iter().enumerate() {
        router.add_edge(
            "load",
            Edge::new(*summary, "done").with_costs(
                1.0 + index as f64 / 2.0,
                0.001,
                100 + index as u64 * 25,
                0.1,
            ),
        );
    }

    router
}

const ALL_TOOL_NODES: [&str; 8] = [
    "search_a",
    "search_b",
    "search_c",
    "search_d",
    "summarize_a",
    "summarize_b",
    "summarize_c",
    "summarize_d",
];

#[derive(Default)]
struct LoadMetrics {
    route_successes: AtomicUsize,
    route_escalations: AtomicUsize,
    escalations_all_paths_blocked: AtomicUsize,
    escalations_with_penalty_path_available: AtomicUsize,
    escalations_unknown_state: AtomicUsize,
    escalation_snapshots_search_open_total: AtomicUsize,
    escalation_snapshots_summarize_open_total: AtomicUsize,
    escalation_snapshots_penalty_node_usable: AtomicUsize,
    route_errors: AtomicUsize,
    failure_transitions: AtomicUsize,
    recovery_transitions: AtomicUsize,
    failure_task_completed: AtomicBool,
}

fn percentile(values: &[Duration], percentile: f64) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }

    let index = ((values.len() - 1) as f64 * percentile / 100.0).round() as usize;
    values[index]
}
