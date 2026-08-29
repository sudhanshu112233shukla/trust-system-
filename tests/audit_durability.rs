use std::fs;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn audit_jsonl_survives_process_kill_without_corruption() {
    let writer = std::env::var("CARGO_BIN_EXE_trust-router-audit-crash-writer")
        .expect("crash writer binary path missing");
    let path = std::env::temp_dir().join(format!(
        "trust-router-audit-crash-{}.jsonl",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);

    let mut child = Command::new(writer)
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn crash writer");

    wait_for_completed_lines(&path, 5);
    child.kill().expect("failed to kill crash writer");
    let _ = child.wait();

    let contents = fs::read_to_string(&path).expect("audit file should exist after kill");
    let lines: Vec<_> = contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();

    assert!(
        lines.len() >= 5,
        "expected at least 5 completed audit lines, got {}",
        lines.len()
    );

    for (index, line) in lines.iter().enumerate() {
        assert_valid_audit_json_line(index, line);
    }

    let _ = fs::remove_file(path);
}

fn wait_for_completed_lines(path: &std::path::Path, minimum_lines: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);

    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(path) {
            if contents.lines().count() >= minimum_lines {
                return;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }

    panic!("audit writer did not produce {minimum_lines} completed lines before timeout");
}

fn assert_valid_audit_json_line(index: usize, line: &str) {
    assert!(
        line.starts_with('{') && line.ends_with('}'),
        "line {index} is not a complete JSON object: {line}"
    );
    assert!(
        line.contains("\"event\":\"reroute\"")
            || line.contains("\"event\":\"explicit_escalation\"")
            || line.contains("\"event\":\"health_changed\"")
            || line.contains("\"event\":\"recovery_probe_opened\""),
        "line {index} does not contain a known audit event: {line}"
    );
    assert!(
        !line.as_bytes().contains(&0),
        "line {index} contains NUL bytes"
    );
    assert!(
        has_balanced_json_string_quotes(line),
        "line {index} has unbalanced JSON string quotes: {line}"
    );
}

fn has_balanced_json_string_quotes(line: &str) -> bool {
    let mut escaped = false;
    let mut in_string = false;

    for ch in line.chars() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            _ => {}
        }
    }

    !in_string && !escaped
}
