use std::process::Command;

#[test]
fn production_mode_requires_an_explicit_non_demo_api_key() {
    let binary =
        std::env::var("CARGO_BIN_EXE_trust-router-sidecar").expect("sidecar binary path missing");
    let output = Command::new(binary)
        .env("TRUST_ROUTER_MODE", "production")
        .env_remove("TRUST_ROUTER_API_KEY")
        .env_remove("TRUST_ROUTER_API_KEY_FILE")
        .env_remove("TRUST_ROUTER_CONFIG")
        .output()
        .expect("failed to run sidecar");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("production mode requires"), "{stderr}");
}
