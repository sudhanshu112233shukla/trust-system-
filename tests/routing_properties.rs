use std::collections::HashSet;

use proptest::prelude::*;
use trust_router::{Edge, OPEN_THRESHOLD, RouteDecision, Router};

proptest! {
    #[test]
    fn routed_paths_use_valid_edges_and_are_deterministic(
        primary_base in 0.01f64..25.0,
        fallback_base in 0.01f64..25.0,
        primary_latency in 1u64..5_000,
        fallback_latency in 1u64..5_000,
    ) {
        let first = router_with_two_paths(primary_base, fallback_base, primary_latency, fallback_latency);
        let second = router_with_two_paths(primary_base, fallback_base, primary_latency, fallback_latency);
        let valid_edges = valid_two_path_edges();

        let first_route = expect_routed(first.route("property", "start", "done"));
        let second_route = expect_routed(second.route("property", "start", "done"));

        prop_assert_eq!(&first_route.nodes, &second_route.nodes);
        prop_assert!((first_route.total_cost - second_route.total_cost).abs() < f64::EPSILON);
        prop_assert!(path_uses_only_edges(&first_route.nodes, &valid_edges));
        prop_assert_eq!(first_route.nodes.first().map(String::as_str), Some("start"));
        prop_assert_eq!(first_route.nodes.last().map(String::as_str), Some("done"));
    }

    #[test]
    fn open_nodes_are_never_selected_when_an_alternative_exists(
        fallback_base in 0.01f64..25.0,
        fallback_latency in 1u64..5_000,
    ) {
        let router = router_with_two_paths(0.01, fallback_base, 1, fallback_latency);
        for _ in 0..OPEN_THRESHOLD {
            router.record_result("property", "primary", false, 30_000);
        }

        let route = expect_routed(router.route("property", "start", "done"));
        prop_assert!(!route.nodes.iter().any(|node| node == "primary"));
        prop_assert!(route.nodes.iter().any(|node| node == "fallback"));
    }

    #[test]
    fn cached_and_uncached_routes_are_equivalent(
        primary_base in 0.01f64..25.0,
        fallback_base in 0.01f64..25.0,
    ) {
        let router = router_with_two_paths(primary_base, fallback_base, 100, 200);
        let uncached = expect_routed(router.route("property", "start", "done"));
        let cached = expect_routed(router.route("property", "start", "done"));

        prop_assert!(!uncached.cache_hit);
        prop_assert!(cached.cache_hit);
        prop_assert_eq!(uncached.nodes, cached.nodes);
        prop_assert!((uncached.total_cost - cached.total_cost).abs() < f64::EPSILON);
    }
}

#[test]
fn disabled_edges_are_not_used() {
    let router = Router::new();
    router.add_tenant("property");
    router.add_edge(
        "property",
        Edge::new("start", "primary").with_costs(0.01, 0.0, 1, 0.0),
    );
    let mut disabled_primary_done = Edge::new("primary", "done").with_costs(0.01, 0.0, 1, 0.0);
    disabled_primary_done.enabled = false;
    router.add_edge("property", disabled_primary_done);
    router.add_edge(
        "property",
        Edge::new("start", "fallback").with_costs(10.0, 0.0, 100, 0.0),
    );
    router.add_edge(
        "property",
        Edge::new("fallback", "done").with_costs(10.0, 0.0, 100, 0.0),
    );

    let route = expect_routed(router.route("property", "start", "done"));
    assert_eq!(route.nodes, vec!["start", "fallback", "done"]);
}

#[test]
fn invalid_floating_point_edges_are_not_routed() {
    let invalid_costs = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0];
    for invalid in invalid_costs {
        let router = Router::new();
        router.add_tenant("property");
        router.add_edge(
            "property",
            Edge::new("start", "bad").with_costs(invalid, 0.0, 1, 0.0),
        );
        router.add_edge(
            "property",
            Edge::new("bad", "done").with_costs(1.0, 0.0, 1, 0.0),
        );

        assert!(matches!(
            router.route("property", "start", "done"),
            RouteDecision::Escalate(_)
        ));
    }
}

fn router_with_two_paths(
    primary_base: f64,
    fallback_base: f64,
    primary_latency: u64,
    fallback_latency: u64,
) -> Router {
    let router = Router::new();
    router.add_tenant("property");
    router.add_edge(
        "property",
        Edge::new("start", "primary").with_costs(primary_base, 0.001, primary_latency, 0.1),
    );
    router.add_edge(
        "property",
        Edge::new("primary", "done").with_costs(primary_base, 0.001, primary_latency, 0.1),
    );
    router.add_edge(
        "property",
        Edge::new("start", "fallback").with_costs(fallback_base, 0.002, fallback_latency, 0.2),
    );
    router.add_edge(
        "property",
        Edge::new("fallback", "done").with_costs(fallback_base, 0.002, fallback_latency, 0.2),
    );
    router
}

fn valid_two_path_edges() -> HashSet<(String, String)> {
    [
        ("start".to_string(), "primary".to_string()),
        ("primary".to_string(), "done".to_string()),
        ("start".to_string(), "fallback".to_string()),
        ("fallback".to_string(), "done".to_string()),
    ]
    .into_iter()
    .collect()
}

fn path_uses_only_edges(path: &[String], valid_edges: &HashSet<(String, String)>) -> bool {
    path.windows(2)
        .all(|pair| valid_edges.contains(&(pair[0].clone(), pair[1].clone())))
}

fn expect_routed(decision: RouteDecision) -> trust_router::Route {
    match decision {
        RouteDecision::Routed(route) => route,
        RouteDecision::Escalate(context) => panic!("expected route, got escalation: {context:?}"),
    }
}
