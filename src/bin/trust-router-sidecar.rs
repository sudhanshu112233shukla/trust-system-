use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use trust_router::{AuditEvent, Edge, RouteDecision, Router, append_events_jsonl};

const DEFAULT_API_KEY: &str = "trust-router-demo-key";
const KNOWN_NODES: [&str; 5] = [
    "start",
    "primary_search",
    "fallback_search",
    "summarize",
    "done",
];

fn main() -> std::io::Result<()> {
    let address = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:7878".to_string());
    let audit_path = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("sidecar-audit.jsonl"));
    let api_key =
        std::env::var("TRUST_ROUTER_API_KEY").unwrap_or_else(|_| DEFAULT_API_KEY.to_string());
    let router = build_demo_router();
    let metrics = Arc::new(Mutex::new(Metrics::default()));
    let listener = TcpListener::bind(&address)?;

    println!("Trust Router sidecar listening on http://{address}");
    println!("Audit file: {}", audit_path.display());
    println!("Protected endpoints require X-API-Key.");
    println!("Try: http://{address}/route?tenant=yc-demo&start=start&goal=done");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_connection(stream, &router, &audit_path, &metrics, &api_key),
            Err(error) => eprintln!("connection failed: {error}"),
        }
    }

    Ok(())
}

fn handle_connection(
    mut stream: TcpStream,
    router: &Router,
    audit_path: &PathBuf,
    metrics: &Arc<Mutex<Metrics>>,
    api_key: &str,
) {
    let mut buffer = [0; 2048];
    let Ok(read) = stream.read(&mut buffer) else {
        return;
    };
    if read == 0 {
        return;
    }

    let request = String::from_utf8_lossy(&buffer[..read]);
    let Some(request_line) = request.lines().next() else {
        write_error_response(
            &mut stream,
            SidecarError::BadRequest("missing request line".to_string()),
        );
        return;
    };

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let (path, query) = split_target(target);
    let headers = parse_headers(&request);

    let response = match (method, path) {
        ("GET", "/healthz") => Ok("{\"ok\":true}".to_string()),
        ("GET", "/metrics") => {
            require_api_key(&headers, api_key).and_then(|_| handle_metrics(router, metrics))
        }
        ("GET" | "POST", "/route") => require_api_key(&headers, api_key)
            .and_then(|_| handle_route(router, audit_path, metrics, &query)),
        ("GET" | "POST", "/result") => {
            require_api_key(&headers, api_key).and_then(|_| handle_result(router, &query))
        }
        ("GET" | "POST", "/force-half-open") => {
            require_api_key(&headers, api_key).and_then(|_| handle_force_half_open(router, &query))
        }
        _ => Err(SidecarError::NotFound(format!(
            "unknown endpoint: {method} {path}"
        ))),
    };

    match response {
        Ok(body) => write_response(&mut stream, 200, &body),
        Err(error) => write_error_response(&mut stream, error),
    }
}

#[derive(Debug)]
enum SidecarError {
    BadRequest(String),
    Unauthorized,
    NotFound(String),
}

impl SidecarError {
    fn status(&self) -> u16 {
        match self {
            SidecarError::BadRequest(_) => 400,
            SidecarError::Unauthorized => 401,
            SidecarError::NotFound(_) => 404,
        }
    }

    fn message(&self) -> &str {
        match self {
            SidecarError::BadRequest(message) | SidecarError::NotFound(message) => message,
            SidecarError::Unauthorized => "missing or invalid API key",
        }
    }
}

fn handle_route(
    router: &Router,
    audit_path: &PathBuf,
    metrics: &Arc<Mutex<Metrics>>,
    query: &HashMap<String, String>,
) -> Result<String, SidecarError> {
    let tenant = required(query, "tenant")?;
    let start = required(query, "start")?;
    let goal = required(query, "goal")?;
    let started = Instant::now();
    let decision = router.route(tenant, start, goal);
    let elapsed = started.elapsed();
    let event = router.last_audit_event().ok_or_else(|| {
        SidecarError::BadRequest("route did not create an audit event".to_string())
    })?;
    persist_before_response(audit_path, event)?;

    let mut metrics = metrics.lock().expect("metrics lock poisoned");
    metrics.record_route(&decision, elapsed);
    drop(metrics);

    Ok(route_decision_json(decision))
}

fn handle_result(router: &Router, query: &HashMap<String, String>) -> Result<String, SidecarError> {
    let tenant = required(query, "tenant")?;
    let node = required(query, "node")?;
    let success = required(query, "success")?
        .parse::<bool>()
        .map_err(|_| SidecarError::BadRequest("success must be true or false".to_string()))?;
    let latency_ms = required(query, "latency_ms")?
        .parse::<u64>()
        .map_err(|_| SidecarError::BadRequest("latency_ms must be an integer".to_string()))?;

    router.record_result(tenant, node, success, latency_ms);
    Ok("{\"ok\":true}".to_string())
}

fn handle_force_half_open(
    router: &Router,
    query: &HashMap<String, String>,
) -> Result<String, SidecarError> {
    let tenant = required(query, "tenant")?;
    let node = required(query, "node")?;
    let changed = router.force_half_open(tenant, node);
    Ok(format!("{{\"changed\":{changed}}}"))
}

fn handle_metrics(router: &Router, metrics: &Arc<Mutex<Metrics>>) -> Result<String, SidecarError> {
    let metrics = metrics.lock().expect("metrics lock poisoned");
    Ok(metrics.to_json(router, "yc-demo"))
}

fn route_decision_json(decision: RouteDecision) -> String {
    match decision {
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
    }
}

#[derive(Debug, Default)]
struct Metrics {
    total_routes: u64,
    total_reroutes: u64,
    total_escalations: u64,
    route_latencies: Vec<Duration>,
}

impl Metrics {
    fn record_route(&mut self, decision: &RouteDecision, latency: Duration) {
        self.total_routes += 1;
        match decision {
            RouteDecision::Routed(_) => self.total_reroutes += 1,
            RouteDecision::Escalate(_) => self.total_escalations += 1,
        }
        self.route_latencies.push(latency);
    }

    fn to_json(&self, router: &Router, tenant: &str) -> String {
        let escalation_rate = if self.total_routes == 0 {
            0.0
        } else {
            self.total_escalations as f64 / self.total_routes as f64
        };
        let llm_calls_avoided = self.total_routes.saturating_sub(self.total_escalations);
        let mut sorted = self.route_latencies.clone();
        sorted.sort_unstable();

        format!(
            "{{\"total_routes\":{},\"total_reroutes\":{},\"total_escalations\":{},\"escalation_rate\":{},\"llm_calls_avoided\":{},\"node_health\":{},\"route_latency_us\":{{\"p50\":{},\"p95\":{},\"p99\":{}}}}}",
            self.total_routes,
            self.total_reroutes,
            self.total_escalations,
            escalation_rate,
            llm_calls_avoided,
            node_health_json(router, tenant),
            percentile_us(&sorted, 50.0),
            percentile_us(&sorted, 95.0),
            percentile_us(&sorted, 99.0)
        )
    }
}

fn node_health_json(router: &Router, tenant: &str) -> String {
    let items: Vec<_> = KNOWN_NODES
        .iter()
        .map(|node| {
            let state = router
                .node_state(tenant, node)
                .map(|state| format!("{state:?}"))
                .unwrap_or_else(|| "Healthy".to_string());
            format!("\"{}\":\"{}\"", json_escape(node), state)
        })
        .collect();
    format!("{{{}}}", items.join(","))
}

fn percentile_us(values: &[Duration], percentile: f64) -> u128 {
    if values.is_empty() {
        return 0;
    }

    let index = ((values.len() - 1) as f64 * percentile / 100.0).round() as usize;
    values[index].as_micros()
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

fn persist_before_response(path: &PathBuf, event: AuditEvent) -> Result<(), SidecarError> {
    append_events_jsonl(path, &[event])
        .map(|_| ())
        .map_err(|error| SidecarError::BadRequest(format!("audit persistence failed: {error}")))
}

fn split_target(target: &str) -> (&str, HashMap<String, String>) {
    let Some((path, query)) = target.split_once('?') else {
        return (target, HashMap::new());
    };

    let query = query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((url_decode(key), url_decode(value)))
        })
        .collect();

    (path, query)
}

fn required<'a>(query: &'a HashMap<String, String>, key: &str) -> Result<&'a str, SidecarError> {
    query
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| SidecarError::BadRequest(format!("missing query parameter: {key}")))
}

fn parse_headers(request: &str) -> HashMap<String, String> {
    request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect()
}

fn require_api_key(headers: &HashMap<String, String>, expected: &str) -> Result<(), SidecarError> {
    let Some(actual) = headers.get("x-api-key") else {
        return Err(SidecarError::Unauthorized);
    };

    constant_time_eq(actual.as_bytes(), expected.as_bytes())
        .then_some(())
        .ok_or(SidecarError::Unauthorized)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();

    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= (left_byte ^ right_byte) as usize;
    }

    diff == 0
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn write_error_response(stream: &mut TcpStream, error: SidecarError) {
    write_response(
        stream,
        error.status(),
        &format!("{{\"error\":\"{}\"}}", json_escape(error.message())),
    );
}

fn json_string_array(values: &[String]) -> String {
    let items: Vec<_> = values
        .iter()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .collect();
    format!("[{}]", items.join(","))
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn url_decode(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        match ch {
            '+' => output.push(' '),
            '%' => {
                let hi = chars.next();
                let lo = chars.next();
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    if let Ok(byte) = u8::from_str_radix(&format!("{hi}{lo}"), 16) {
                        output.push(byte as char);
                    }
                }
            }
            _ => output.push(ch),
        }
    }

    output
}
