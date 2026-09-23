use trust_router::{Edge, OPEN_THRESHOLD, RouteDecision, Router};

const RUNS: usize = 30;

#[derive(Debug, Clone, Copy)]
struct RunResult {
    success: bool,
    llm_calls: f64,
}

#[derive(Debug)]
struct Summary {
    success_rate: f64,
    mean_llm_calls: f64,
    stddev_llm_calls: f64,
}

fn main() {
    let react: Vec<_> = (0..RUNS).map(run_pure_react).collect();
    let routed: Vec<_> = (0..RUNS).map(run_trust_router).collect();
    let react_summary = summarize(&react);
    let routed_summary = summarize(&routed);
    let reduction = if react_summary.mean_llm_calls == 0.0 {
        0.0
    } else {
        (1.0 - routed_summary.mean_llm_calls / react_summary.mean_llm_calls) * 100.0
    };

    println!("Trust Router statistical baseline");
    println!("Runs per arm: {RUNS}");
    println!();
    print_summary("Pure ReAct control loop", &react_summary);
    print_summary("Trust Router", &routed_summary);
    println!();
    println!("Control-plane LLM call reduction: {reduction:.1}%");
}

fn run_pure_react(seed: usize) -> RunResult {
    let primary_down = seed.is_multiple_of(5);
    let fallback_down = seed.is_multiple_of(11);
    let mut llm_calls = 1.0;

    llm_calls += 1.0;
    if !primary_down {
        llm_calls += 1.0;
        return RunResult {
            success: true,
            llm_calls,
        };
    }

    llm_calls += 1.0;
    if !fallback_down {
        llm_calls += 1.0;
        return RunResult {
            success: true,
            llm_calls,
        };
    }

    llm_calls += 1.0;
    RunResult {
        success: false,
        llm_calls,
    }
}

fn run_trust_router(seed: usize) -> RunResult {
    let router = build_router();
    let primary_down = seed.is_multiple_of(5);
    let fallback_down = seed.is_multiple_of(11);

    if primary_down {
        open_node(&router, "primary_search");
    }
    if fallback_down {
        open_node(&router, "fallback_search");
    }

    match router.route("baseline", "start", "done") {
        RouteDecision::Routed(_) => RunResult {
            success: true,
            llm_calls: 0.0,
        },
        RouteDecision::Escalate(_) => RunResult {
            success: false,
            llm_calls: 1.0,
        },
    }
}

fn build_router() -> Router {
    let router = Router::new();
    router.add_tenant("baseline");
    router.add_edge(
        "baseline",
        Edge::new("start", "primary_search").with_costs(1.0, 0.001, 120, 0.1),
    );
    router.add_edge(
        "baseline",
        Edge::new("primary_search", "summarize").with_costs(1.0, 0.002, 320, 0.2),
    );
    router.add_edge(
        "baseline",
        Edge::new("start", "fallback_search").with_costs(10.0, 0.01, 1_000, 1.0),
    );
    router.add_edge(
        "baseline",
        Edge::new("fallback_search", "summarize").with_costs(5.0, 0.005, 800, 0.5),
    );
    router.add_edge(
        "baseline",
        Edge::new("summarize", "done").with_costs(1.0, 0.001, 120, 0.1),
    );
    router
}

fn open_node(router: &Router, node_id: &str) {
    for _ in 0..OPEN_THRESHOLD {
        router.record_result("baseline", node_id, false, 1_000);
    }
}

fn summarize(results: &[RunResult]) -> Summary {
    let success_rate =
        results.iter().filter(|result| result.success).count() as f64 / results.len() as f64;
    let mean_llm_calls =
        results.iter().map(|result| result.llm_calls).sum::<f64>() / results.len() as f64;
    let variance = results
        .iter()
        .map(|result| (result.llm_calls - mean_llm_calls).powi(2))
        .sum::<f64>()
        / results.len() as f64;

    Summary {
        success_rate,
        mean_llm_calls,
        stddev_llm_calls: variance.sqrt(),
    }
}

fn print_summary(label: &str, summary: &Summary) {
    println!("{label}");
    println!("   success_rate={:.1}%", summary.success_rate * 100.0);
    println!("   mean_llm_calls={:.2}", summary.mean_llm_calls);
    println!("   stddev_llm_calls={:.2}", summary.stddev_llm_calls);
}
