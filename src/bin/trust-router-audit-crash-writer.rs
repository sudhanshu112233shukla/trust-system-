use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use trust_router::{Edge, Router, append_events_jsonl};

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("audit-crash-test.jsonl"));
    let router = build_router();

    for _ in 0..1_000 {
        let _ = router.route("audit-crash", "start", "done");
        let event = router
            .last_audit_event()
            .expect("route should always create audit event");
        append_events_jsonl(&path, &[event]).expect("audit write should succeed");
        thread::sleep(Duration::from_millis(5));
    }
}

fn build_router() -> Router {
    let router = Router::new();
    router.add_tenant("audit-crash");
    router.add_edge(
        "audit-crash",
        Edge::new("start", "search").with_costs(1.0, 0.001, 100, 0.1),
    );
    router.add_edge(
        "audit-crash",
        Edge::new("search", "done").with_costs(1.0, 0.001, 100, 0.1),
    );
    router
}
