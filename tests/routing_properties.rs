use proptest::prelude::*;
use trust_router::{AuditEvent, Edge, RouteDecision, Router};

proptest! {
    #[test]
    fn generated_cost_graphs_never_route_silently(
        primary_base in 0.01f64..100.0,
        fallback_base in 0.01f64..100.0,
        primary_latency in 1u64..10_000,
        fallback_latency in 1u64..10_000,
    ) {
        let router = Router::new();
        router.add_tenant("property");
        router.add_edge("property", Edge::new("start", "primary").with_costs(primary_base, 0.001, primary_latency, 0.1));
        router.add_edge("property", Edge::new("primary", "done").with_costs(primary_base, 0.001, primary_latency, 0.1));
        router.add_edge("property", Edge::new("start", "fallback").with_costs(fallback_base, 0.002, fallback_latency, 0.2));
        router.add_edge("property", Edge::new("fallback", "done").with_costs(fallback_base, 0.002, fallback_latency, 0.2));

        let decision = router.route("property", "start", "done");

        prop_assert!(matches!(decision, RouteDecision::Routed(_) | RouteDecision::Escalate(_)));
        let has_audit = matches!(
            router.last_audit_event(),
            Some(AuditEvent::Reroute { .. } | AuditEvent::ExplicitEscalation(_))
        );
        prop_assert!(has_audit);
    }
}
