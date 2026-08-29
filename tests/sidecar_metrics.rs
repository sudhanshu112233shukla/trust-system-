use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const OPEN_THRESHOLD: usize = 5;

#[test]
fn sidecar_metrics_counts_routes_reroutes_escalations_and_health() {
    let port = reserve_port();
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let audit_path = std::env::temp_dir().join(format!(
        "trust-router-sidecar-metrics-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&audit_path);
    let mut sidecar = start_sidecar(address, &audit_path);

    wait_for_health(address);

    assert!(
        http_get(address, "/route?tenant=yc-demo&start=start&goal=done")
            .contains("\"decision\":\"routed\"")
    );
    assert!(
        http_get(address, "/route?tenant=yc-demo&start=start&goal=done")
            .contains("\"decision\":\"routed\"")
    );

    for _ in 0..OPEN_THRESHOLD {
        assert!(
            http_get(
                address,
                "/result?tenant=yc-demo&node=primary_search&success=false&latency_ms=30000"
            )
            .contains("\"ok\":true")
        );
    }

    let reroute = http_get(address, "/route?tenant=yc-demo&start=start&goal=done");
    assert!(reroute.contains("\"decision\":\"routed\""));
    assert!(reroute.contains("fallback_search"));

    for _ in 0..OPEN_THRESHOLD {
        assert!(
            http_get(
                address,
                "/result?tenant=yc-demo&node=fallback_search&success=false&latency_ms=30000"
            )
            .contains("\"ok\":true")
        );
    }

    assert!(
        http_get(address, "/route?tenant=yc-demo&start=start&goal=done")
            .contains("\"decision\":\"escalate\"")
    );

    let metrics = http_get(address, "/metrics");
    assert_eq!(json_u64(&metrics, "total_routes"), 4);
    assert_eq!(json_u64(&metrics, "total_reroutes"), 3);
    assert_eq!(json_u64(&metrics, "total_escalations"), 1);
    assert_eq!(json_u64(&metrics, "llm_calls_avoided"), 3);
    assert!(metrics.contains("\"escalation_rate\":0.25"));
    assert!(metrics.contains("\"primary_search\":\"Open\""));
    assert!(metrics.contains("\"fallback_search\":\"Open\""));
    assert!(metrics.contains("\"summarize\":\"Healthy\""));
    assert!(metrics.contains("\"p50\":"));
    assert!(metrics.contains("\"p95\":"));
    assert!(metrics.contains("\"p99\":"));

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
        if http_get(address, "/healthz").contains("\"ok\":true") {
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

fn http_get(address: SocketAddr, target: &str) -> String {
    let Ok(mut stream) = TcpStream::connect(address) else {
        return String::new();
    };
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {address}\r\nX-API-Key: test-key\r\nConnection: close\r\n\r\n"
    );
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

fn json_u64(body: &str, key: &str) -> u64 {
    let marker = format!("\"{key}\":");
    let Some((_, tail)) = body.split_once(&marker) else {
        panic!("missing key {key} in {body}");
    };
    tail.chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse()
        .expect("invalid integer value")
}
