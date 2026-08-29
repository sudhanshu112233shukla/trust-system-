use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn sidecar_requires_api_key_except_healthz() {
    let port = reserve_port();
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let audit_path = std::env::temp_dir().join(format!(
        "trust-router-sidecar-auth-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&audit_path);
    let mut sidecar = start_sidecar(address, &audit_path);

    wait_for_health(address);

    let health = http_get(address, "/healthz", None);
    assert!(health.starts_with("HTTP/1.1 200 OK"), "{health}");
    assert!(health.contains("{\"ok\":true}"), "{health}");

    let without_key = http_get(address, "/route?tenant=yc-demo&start=start&goal=done", None);
    assert!(
        without_key.starts_with("HTTP/1.1 401 Unauthorized"),
        "{without_key}"
    );

    let with_key = http_get(
        address,
        "/route?tenant=yc-demo&start=start&goal=done",
        Some("test-key"),
    );
    assert!(with_key.starts_with("HTTP/1.1 200 OK"), "{with_key}");
    assert!(with_key.contains("\"decision\":\"routed\""), "{with_key}");

    stop_sidecar(&mut sidecar);
    let _ = std::fs::remove_file(audit_path);
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to reserve port");
    listener.local_addr().expect("missing local addr").port()
}

fn start_sidecar(address: SocketAddr, audit_path: &std::path::Path) -> Child {
    let binary =
        std::env::var("CARGO_BIN_EXE_trust-router-sidecar").expect("sidecar binary path missing");
    Command::new(binary)
        .env("TRUST_ROUTER_API_KEY", "test-key")
        .arg(address.to_string())
        .arg(audit_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start sidecar")
}

fn wait_for_health(address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if http_get(address, "/healthz", None).contains("\"ok\":true") {
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

fn http_get(address: SocketAddr, target: &str, api_key: Option<&str>) -> String {
    let Ok(mut stream) = TcpStream::connect(address) else {
        return String::new();
    };
    let api_key_header = api_key
        .map(|key| format!("X-API-Key: {key}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {address}\r\n{api_key_header}Connection: close\r\n\r\n"
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
