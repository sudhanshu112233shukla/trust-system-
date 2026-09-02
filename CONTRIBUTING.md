# Contributing

## Verify Before Sending Changes

Run the full verifier:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\verify.ps1
```

For focused Rust checks:

```powershell
cargo fmt --check
cargo test
cargo run --bin trust-router-loadtest
```

## Project Conventions

- Keep the routing core deterministic. A route request must either return a route or emit an explicit escalation.
- Preserve the never-silent invariant: no failed route should disappear without an audit event.
- `Degraded` and `HalfOpen` nodes remain routable with penalties. Only `Open` removes a node from path search.
- Cache invalidation should stay node-scoped unless a broader invalidation is required for correctness.
- Persist audit events before returning route decisions from the sidecar.
- Treat audit JSONL as the trust record. Operational logs are for debugging and should not replace audit events.
- Keep SDK examples small and executable against the local sidecar.
