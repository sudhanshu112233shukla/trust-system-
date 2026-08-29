use std::path::PathBuf;

use trust_router::{AuditEvent, Edge, OPEN_THRESHOLD, RouteDecision, Router};

fn main() {
    let audit_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("demo-audit.jsonl"));
    let router = build_demo_router();

    println!("Trust Router demo");
    println!("Audit file: {}", audit_path.display());
    println!();

    print_decision(
        "1. Normal path: deterministic route, no LLM needed",
        router.route("yc-demo", "start", "done"),
    );
    print_decision(
        "2. Same request: cached path reused",
        router.route("yc-demo", "start", "done"),
    );

    open_node(&router, "primary_search");
    print_decision(
        "3. Primary search opens: router takes fallback path",
        router.route("yc-demo", "start", "done"),
    );

    open_node(&router, "fallback_search");
    print_decision(
        "4. Compound failure: no feasible path, explicit LLM escalation",
        router.route("yc-demo", "start", "done"),
    );

    router.force_half_open("yc-demo", "primary_search");
    print_decision(
        "5. Recovery probe: half-open primary is cautiously routable again",
        router.route("yc-demo", "start", "done"),
    );

    match router.append_audit_jsonl(&audit_path) {
        Ok(count) => println!("\nPersisted {count} audit events with fsync."),
        Err(error) => {
            eprintln!("\nFailed to persist audit events: {error}");
            std::process::exit(1);
        }
    }

    print_summary(router.events());
}

fn build_demo_router() -> Router {
    let router = Router::new();
    router.add_tenant("yc-demo");
    router.add_edge(
        "yc-demo",
        Edge::new("start", "primary_search").with_costs(1.0, 0.001, 120, 0.1),
    );
    router.add_edge(
        "yc-demo",
        Edge::new("primary_search", "summarize").with_costs(1.0, 0.002, 320, 0.2),
    );
    router.add_edge(
        "yc-demo",
        Edge::new("start", "fallback_search").with_costs(10.0, 0.01, 1_000, 1.0),
    );
    router.add_edge(
        "yc-demo",
        Edge::new("fallback_search", "summarize").with_costs(5.0, 0.005, 800, 0.5),
    );
    router.add_edge(
        "yc-demo",
        Edge::new("summarize", "done").with_costs(1.0, 0.001, 120, 0.1),
    );
    router
}

fn open_node(router: &Router, node_id: &str) {
    for _ in 0..OPEN_THRESHOLD {
        router.record_result("yc-demo", node_id, false, 1_000);
    }
}

fn print_decision(label: &str, decision: RouteDecision) {
    println!("{label}");
    match decision {
        RouteDecision::Routed(route) => {
            println!(
                "   routed path={} cost={:.3} cache_hit={}",
                route.nodes.join(" -> "),
                route.total_cost,
                route.cache_hit
            );
        }
        RouteDecision::Escalate(context) => {
            println!(
                "   escalation reason={:?} blocked={}",
                context.reason,
                context.failed_or_blocked_nodes.join(", ")
            );
        }
    }
    println!();
}

fn print_summary(events: Vec<AuditEvent>) {
    let reroutes = events
        .iter()
        .filter(|event| matches!(event, AuditEvent::Reroute { .. }))
        .count();
    let escalations = events
        .iter()
        .filter(|event| matches!(event, AuditEvent::ExplicitEscalation(_)))
        .count();
    let health_changes = events
        .iter()
        .filter(|event| matches!(event, AuditEvent::HealthChanged { .. }))
        .count();
    let recovery_probes = events
        .iter()
        .filter(|event| matches!(event, AuditEvent::RecoveryProbeOpened { .. }))
        .count();

    println!("Demo metrics");
    println!("   reroutes={reroutes}");
    println!("   escalations={escalations}");
    println!("   health_events={health_changes}");
    println!("   recovery_probes={recovery_probes}");
}
