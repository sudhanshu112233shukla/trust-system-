use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let address = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:7979".to_string());
    let listener = TcpListener::bind(&address)?;
    let counter = Arc::new(AtomicU64::new(0));

    println!("Trust Router mock tool server listening on http://{address}");
    println!(
        "Try: http://{address}/invoke?tool=primary_search&fail_pct=10&slow_pct=5&min_latency_ms=10&max_latency_ms=50&slow_latency_ms=250"
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let counter = counter.clone();
                thread::spawn(move || handle_connection(stream, counter));
            }
            Err(error) => eprintln!("connection failed: {error}"),
        }
    }

    Ok(())
}

fn handle_connection(mut stream: TcpStream, counter: Arc<AtomicU64>) {
    let mut buffer = [0; 2048];
    let Ok(read) = stream.read(&mut buffer) else {
        return;
    };
    if read == 0 {
        return;
    }

    let request = String::from_utf8_lossy(&buffer[..read]);
    let request_line = request.lines().next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let (path, query) = split_target(target);

    if method != "GET" || path != "/invoke" {
        write_response(&mut stream, 404, "{\"error\":\"not found\"}");
        return;
    }

    let sequence = counter.fetch_add(1, Ordering::Relaxed);
    let fail_pct = parse_u64(&query, "fail_pct", 10).min(100);
    let slow_pct = parse_u64(&query, "slow_pct", 5).min(100);
    let min_latency_ms = parse_u64(&query, "min_latency_ms", 5);
    let max_latency_ms = parse_u64(&query, "max_latency_ms", 50).max(min_latency_ms);
    let slow_latency_ms = parse_u64(&query, "slow_latency_ms", max_latency_ms * 5);
    let roll = pseudo_random(
        sequence,
        query.get("tool").map(String::as_str).unwrap_or("tool"),
    );
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

fn parse_u64(query: &HashMap<String, String>, key: &str, default: u64) -> u64 {
    query
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}
