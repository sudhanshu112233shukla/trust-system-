use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn sidecar_handles_batch_routing_requests() {
    let address = SocketAddr::from(([127, 0, 0, 1], reserve_port()));
    let audit_path = audit_path("batch");
    let mut sidecar = start_sidecar(address, &audit_path);
    wait_for_health(address);

    // 1. Missing API Key on batch
    let unauthorized = post_json(
        address,
        "/route/batch",
        r#"{"requests":[{"tenant":"yc-demo","start":"start","goal":"done"}]}"#,
        None,
    );
    assert!(
        unauthorized.starts_with("HTTP/1.1 401 Unauthorized"),
        "{unauthorized}"
    );

    // 2. Empty batch returns 400 Bad Request
    let empty_batch = post_json(
        address,
        "/route/batch",
        r#"{"requests":[]}"#,
        Some("test-key"),
    );
    assert!(
        empty_batch.starts_with("HTTP/1.1 400 Bad Request"),
        "{empty_batch}"
    );
    assert!(
        empty_batch.contains("batch requests list must not be empty"),
        "{empty_batch}"
    );

    // 3. Unauthorized tenant returns 403 Forbidden
    let forbidden = post_json(
        address,
        "/route/batch",
        r#"{"requests":[{"tenant":"unauthorized-tenant","start":"start","goal":"done"}]}"#,
        Some("test-key"),
    );
    assert!(
        forbidden.starts_with("HTTP/1.1 403 Forbidden"),
        "{forbidden}"
    );

    // 4. Valid batch routing
    let valid_batch = post_json(
        address,
        "/route/batch",
        r#"{"requests":[
            {"tenant":"yc-demo","start":"start","goal":"done"},
            {"tenant":"yc-demo","start":"start","goal":"summarize"}
        ]}"#,
        Some("test-key"),
    );
    assert!(
        valid_batch.starts_with("HTTP/1.1 200 OK"),
        "{valid_batch}"
    );
    assert!(valid_batch.contains("\"total_routed\":2"), "{valid_batch}");
    assert!(valid_batch.contains("\"total_escalated\":0"), "{valid_batch}");
    assert!(valid_batch.contains("\"decision\":\"routed\""), "{valid_batch}");
    assert!(valid_batch.contains("x-request-id:"), "{valid_batch}");

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

fn start_sidecar(address: SocketAddr, audit_path: &std::path::Path) -> Child {
    let binary =
        std::env::var("CARGO_BIN_EXE_trust-router-sidecar").expect("sidecar binary path missing");
    let mut command = Command::new(binary);
    command
        .env("TRUST_ROUTER_API_KEY", "test-key")
        .arg(address.to_string())
        .arg(audit_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().expect("failed to start sidecar")
}

fn wait_for_health(address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let Ok(mut stream) = TcpStream::connect(address) else {
            thread::sleep(Duration::from_millis(50));
            continue;
        };
        let request = format!(
            "GET /healthz HTTP/1.1\r\nHost: {address}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(request.as_bytes());
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        if response.contains("{\"ok\":true}") {
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

fn post_json(address: SocketAddr, target: &str, body: &str, api_key: Option<&str>) -> String {
    let mut stream = TcpStream::connect(address).expect("failed to connect to sidecar");
    let api_key_header = api_key
        .map(|key| format!("X-API-Key: {key}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST {target} HTTP/1.1\r\nHost: {address}\r\n{api_key_header}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
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
