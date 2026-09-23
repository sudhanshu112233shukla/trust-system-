use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn sidecar_enforces_auth_authorization_validation_and_method_contracts() {
    let address = SocketAddr::from(([127, 0, 0, 1], reserve_port()));
    let audit_path = audit_path("auth");
    let mut sidecar = start_sidecar(address, &audit_path, None);
    wait_for_health(address);

    let health = request(address, "GET", "/healthz", None);
    assert!(health.starts_with("HTTP/1.1 200 OK"), "{health}");
    assert!(health.contains("{\"ok\":true}"), "{health}");

    let missing_key = request(
        address,
        "GET",
        "/route?tenant=yc-demo&start=start&goal=done",
        None,
    );
    assert!(
        missing_key.starts_with("HTTP/1.1 401 Unauthorized"),
        "{missing_key}"
    );
    assert!(
        missing_key.contains("\"code\":\"unauthorized\""),
        "{missing_key}"
    );
    assert!(missing_key.contains("x-request-id:"), "{missing_key}");
    assert!(
        missing_key.contains("x-content-type-options: nosniff"),
        "{missing_key}"
    );

    let allowed = request(
        address,
        "GET",
        "/route?tenant=yc-demo&start=start&goal=done",
        Some("test-key"),
    );
    assert!(allowed.starts_with("HTTP/1.1 200 OK"), "{allowed}");
    assert!(allowed.contains("\"decision\":\"routed\""), "{allowed}");

    let forbidden = request(
        address,
        "GET",
        "/route?tenant=other-tenant&start=start&goal=done",
        Some("test-key"),
    );
    assert!(
        forbidden.starts_with("HTTP/1.1 403 Forbidden"),
        "{forbidden}"
    );
    assert!(
        forbidden.contains("\"code\":\"tenant_not_authorized\""),
        "{forbidden}"
    );

    let malformed = request(
        address,
        "GET",
        "/route?tenant=yc-demo%0A&start=start&goal=done",
        Some("test-key"),
    );
    assert!(
        malformed.starts_with("HTTP/1.1 400 Bad Request"),
        "{malformed}"
    );
    assert!(
        malformed.contains("\"code\":\"invalid_request\""),
        "{malformed}"
    );

    let mutation_get = request(
        address,
        "GET",
        "/result?tenant=yc-demo&node=primary_search&success=false&latency_ms=10",
        Some("test-key"),
    );
    assert!(
        mutation_get.starts_with("HTTP/1.1 405 Method Not Allowed"),
        "{mutation_get}"
    );

    stop_sidecar(&mut sidecar);
    let _ = std::fs::remove_file(audit_path);
}

#[test]
fn sidecar_rate_limits_authenticated_requests() {
    let address = SocketAddr::from(([127, 0, 0, 1], reserve_port()));
    let audit_path = audit_path("rate-limit");
    let mut sidecar = start_sidecar(address, &audit_path, Some((1, 1)));
    wait_for_health(address);

    let first = request(
        address,
        "GET",
        "/route?tenant=yc-demo&start=start&goal=done",
        Some("test-key"),
    );
    assert!(first.starts_with("HTTP/1.1 200 OK"), "{first}");
    let limited = request(
        address,
        "GET",
        "/route?tenant=yc-demo&start=start&goal=done",
        Some("test-key"),
    );
    assert!(
        limited.starts_with("HTTP/1.1 429 Too Many Requests"),
        "{limited}"
    );
    assert!(limited.contains("\"code\":\"rate_limited\""), "{limited}");
    assert!(limited.contains("retry-after: 1"), "{limited}");

    stop_sidecar(&mut sidecar);
    let _ = std::fs::remove_file(audit_path);
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to reserve port");
    listener.local_addr().expect("missing local addr").port()
}

fn audit_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "trust-router-sidecar-{label}-{}.jsonl",
        std::process::id()
    ))
}

fn start_sidecar(
    address: SocketAddr,
    audit_path: &std::path::Path,
    rate_limit: Option<(u32, u32)>,
) -> Child {
    let binary =
        std::env::var("CARGO_BIN_EXE_trust-router-sidecar").expect("sidecar binary path missing");
    let mut command = Command::new(binary);
    command
        .env("TRUST_ROUTER_API_KEY", "test-key")
        .arg(address.to_string())
        .arg(audit_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some((capacity, refill_per_second)) = rate_limit {
        command
            .env("TRUST_ROUTER_RATE_LIMIT_CAPACITY", capacity.to_string())
            .env(
                "TRUST_ROUTER_RATE_LIMIT_REFILL_PER_SECOND",
                refill_per_second.to_string(),
            );
    }
    command.spawn().expect("failed to start sidecar")
}

fn wait_for_health(address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if request(address, "GET", "/healthz", None).contains("{\"ok\":true}") {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("sidecar did not become healthy");
}

fn stop_sidecar(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn request(address: SocketAddr, method: &str, target: &str, api_key: Option<&str>) -> String {
    let Ok(mut stream) = TcpStream::connect(address) else {
        return String::new();
    };
    let api_key_header = api_key
        .map(|key| format!("X-API-Key: {key}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: {address}\r\n{api_key_header}Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("request write failed");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("response read failed");
    response
}
