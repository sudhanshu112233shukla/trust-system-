use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use trust_router::{AuditEvent, Edge, OPEN_THRESHOLD, RouteDecision, Router};

#[test]
fn http_chaos_trials_never_fail_silently() {
    let router = Router::new();
    let sidecar = TestSidecar::start(router.clone());
    let mock_tools = MockToolServer::start();
    let mut rng = DeterministicRng::new(0xfeed_cafe);

    for trial in 0..200 {
        let tenant = format!("http_chaos_{trial}");
        let node_count = 5 + rng.next_usize(11);
        build_random_graph(&router, &tenant, trial, node_count, &mut rng);
        let failed_count = 1 + rng.next_usize(3);
        let mut failed_nodes = HashSet::new();

        while failed_nodes.len() < failed_count {
            let index = 1 + rng.next_usize(node_count - 2);
            failed_nodes.insert(format!("node_{trial}_{index}"));
        }

        for node in &failed_nodes {
            for _ in 0..OPEN_THRESHOLD {
                let outcome = mock_tools.invoke(node, 100, 0, 1, 2, 75);
                assert!(!outcome.success);
                sidecar.report_result(&tenant, node, outcome.success, outcome.latency_ms);
            }
        }

        let decision = sidecar.route(
            &tenant,
            &format!("node_{trial}_0"),
            &format!("node_{trial}_{}", node_count - 1),
        );

        assert!(
            decision.contains("\"decision\":\"routed\"")
                || decision.contains("\"decision\":\"escalate\""),
            "trial {trial} returned invalid HTTP decision: {decision}"
        );
        assert!(
            matches!(
                router.last_audit_event(),
                Some(AuditEvent::Reroute { .. } | AuditEvent::ExplicitEscalation(_))
            ),
            "trial {trial} failed silently over HTTP after opening nodes {:?}",
            failed_nodes
        );
    }
}

fn build_random_graph(
    router: &Router,
    tenant: &str,
    trial: usize,
    node_count: usize,
    rng: &mut DeterministicRng,
) {
    router.add_tenant(tenant);

    for index in 0..(node_count - 1) {
        router.add_edge(
            tenant,
            Edge::new(
                format!("node_{trial}_{index}"),
                format!("node_{trial}_{}", index + 1),
            )
            .with_costs(
                1.0 + rng.next_usize(5) as f64,
                0.001 * (1 + rng.next_usize(5)) as f64,
                100 + rng.next_usize(400) as u64,
                rng.next_usize(10) as f64 / 10.0,
            ),
        );
    }

    let extra_edges = node_count + rng.next_usize(node_count);
    for _ in 0..extra_edges {
        let from = rng.next_usize(node_count - 1);
        let to = from + 1 + rng.next_usize(node_count - from - 1);
        router.add_edge(
            tenant,
            Edge::new(format!("node_{trial}_{from}"), format!("node_{trial}_{to}")).with_costs(
                1.0 + rng.next_usize(8) as f64,
                0.001 * (1 + rng.next_usize(8)) as f64,
                100 + rng.next_usize(900) as u64,
                rng.next_usize(10) as f64 / 10.0,
            ),
        );
    }
}

struct TestSidecar {
    address: SocketAddr,
}

impl TestSidecar {
    fn start(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("sidecar bind failed");
        let address = listener.local_addr().expect("sidecar address missing");

        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let router = router.clone();
                thread::spawn(move || handle_sidecar_connection(stream, router));
            }
        });

        Self { address }
    }

    fn route(&self, tenant: &str, start: &str, goal: &str) -> String {
        http_get(
            self.address,
            &format!("/route?tenant={tenant}&start={start}&goal={goal}"),
        )
    }

    fn report_result(&self, tenant: &str, node: &str, success: bool, latency_ms: u64) {
        let body = http_get(
            self.address,
            &format!(
                "/result?tenant={tenant}&node={node}&success={success}&latency_ms={latency_ms}"
            ),
        );
        assert!(
            body.contains("\"ok\":true"),
            "unexpected result response: {body}"
        );
    }
}

fn handle_sidecar_connection(mut stream: TcpStream, router: Router) {
    let request = read_request(&mut stream);
    let request_line = request.lines().next().unwrap_or_default();
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
    let (path, query) = split_target(target);

    match path {
        "/route" => {
            let tenant = required(&query, "tenant");
            let start = required(&query, "start");
            let goal = required(&query, "goal");
            let body = match router.route(tenant, start, goal) {
                RouteDecision::Routed(route) => format!(
                    "{{\"decision\":\"routed\",\"path\":{},\"total_cost\":{},\"cache_hit\":{}}}",
                    json_string_array(&route.nodes),
                    route.total_cost,
                    route.cache_hit
                ),
                RouteDecision::Escalate(context) => format!(
                    "{{\"decision\":\"escalate\",\"reason\":\"{:?}\",\"failed_or_blocked_nodes\":{}}}",
                    context.reason,
                    json_string_array(&context.failed_or_blocked_nodes)
                ),
            };
            write_response(&mut stream, 200, &body);
        }
        "/result" => {
            let tenant = required(&query, "tenant");
            let node = required(&query, "node");
            let success = required(&query, "success")
                .parse::<bool>()
                .expect("success should be bool");
            let latency_ms = required(&query, "latency_ms")
                .parse::<u64>()
                .expect("latency_ms should be u64");
            router.record_result(tenant, node, success, latency_ms);
            write_response(&mut stream, 200, "{\"ok\":true}");
        }
        _ => write_response(&mut stream, 404, "{\"error\":\"not found\"}"),
    }
}

struct MockToolServer {
    address: SocketAddr,
}

#[derive(Debug)]
struct ToolOutcome {
    success: bool,
    latency_ms: u64,
}

impl MockToolServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock server bind failed");
        let address = listener.local_addr().expect("mock address missing");
        let counter = Arc::new(AtomicU64::new(0));

        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let counter = counter.clone();
                thread::spawn(move || handle_mock_connection(stream, counter));
            }
        });

        Self { address }
    }

    fn invoke(
        &self,
        tool: &str,
        fail_pct: u64,
        slow_pct: u64,
        min_latency_ms: u64,
        max_latency_ms: u64,
        slow_latency_ms: u64,
    ) -> ToolOutcome {
        let body = http_get(
            self.address,
            &format!(
                "/invoke?tool={tool}&fail_pct={fail_pct}&slow_pct={slow_pct}&min_latency_ms={min_latency_ms}&max_latency_ms={max_latency_ms}&slow_latency_ms={slow_latency_ms}"
            ),
        );
        ToolOutcome {
            success: body.contains("\"success\":true"),
            latency_ms: parse_json_u64(&body, "latency_ms"),
        }
    }
}

fn handle_mock_connection(mut stream: TcpStream, counter: Arc<AtomicU64>) {
    let request = read_request(&mut stream);
    let request_line = request.lines().next().unwrap_or_default();
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
    let (path, query) = split_target(target);

    if path != "/invoke" {
        write_response(&mut stream, 404, "{\"error\":\"not found\"}");
        return;
    }

    let sequence = counter.fetch_add(1, Ordering::Relaxed);
    let tool = query.get("tool").map(String::as_str).unwrap_or("tool");
    let fail_pct = parse_query_u64(&query, "fail_pct", 10).min(100);
    let slow_pct = parse_query_u64(&query, "slow_pct", 5).min(100);
    let min_latency_ms = parse_query_u64(&query, "min_latency_ms", 5);
    let max_latency_ms = parse_query_u64(&query, "max_latency_ms", 50).max(min_latency_ms);
    let slow_latency_ms = parse_query_u64(&query, "slow_latency_ms", max_latency_ms * 5);
    let roll = pseudo_random(sequence, tool);
    let success = roll % 100 >= fail_pct;
    let slow = success && (roll / 100) % 100 < slow_pct;
    let latency_ms = if slow {
        slow_latency_ms
    } else {
        min_latency_ms + (roll % (max_latency_ms - min_latency_ms + 1))
    };

    thread::sleep(Duration::from_millis(latency_ms));
    write_response(
        &mut stream,
        200,
        &format!("{{\"success\":{success},\"latency_ms\":{latency_ms}}}"),
    );
}

fn http_get(address: SocketAddr, target: &str) -> String {
    let mut stream = TcpStream::connect(address).expect("connect failed");
    let request = format!("GET {target} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .expect("request write failed");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("response read failed");
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or(response)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut buffer = [0; 2048];
    let read = stream.read(&mut buffer).unwrap_or(0);
    String::from_utf8_lossy(&buffer[..read]).to_string()
}

fn split_target(target: &str) -> (&str, HashMap<String, String>) {
    let Some((path, query)) = target.split_once('?') else {
        return (target, HashMap::new());
    };

    let query = query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect();

    (path, query)
}

fn required<'a>(query: &'a HashMap<String, String>, key: &str) -> &'a str {
    query.get(key).map(String::as_str).expect("missing query")
}

fn parse_query_u64(query: &HashMap<String, String>, key: &str, default: u64) -> u64 {
    query
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn parse_json_u64(body: &str, key: &str) -> u64 {
    let marker = format!("\"{key}\":");
    let Some((_, tail)) = body.split_once(&marker) else {
        return 0;
    };
    tail.chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn json_string_array(values: &[String]) -> String {
    let items: Vec<_> = values.iter().map(|value| format!("\"{value}\"")).collect();
    format!("[{}]", items.join(","))
}

fn pseudo_random(sequence: u64, tool: &str) -> u64 {
    let mut state = sequence ^ 0x9e37_79b9_7f4a_7c15;
    for byte in tool.bytes() {
        state ^= byte as u64;
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
    }
    state
}

struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_usize(&mut self, upper_exclusive: usize) -> usize {
        assert!(upper_exclusive > 0);
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((self.state >> 32) as usize) % upper_exclusive
    }
}
