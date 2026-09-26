# Trust Router
Deterministic control-plane decisions and high-concurrency inference control for AI-agent and LLM workflows.

## What It Is
Trust Router sits above inference execution engines (vLLM, SGLang, TGI, OpenAI-compatible). It validates authentication, enforces tenant admission and rate limits, captures immutable world-state snapshots, evaluates candidate feasibility, runs counterfactual prediction & ranking, enforces hard SLOs via a single Decision Firewall, compiles immutable ExecutionPlans, executes transport adapters, ingests real execution telemetry, updates EWMA prediction models, and maintains tamper-evident audit logs.

## Design Principles
- **Deterministic Safety**: Identical inputs produce identical execution plans. No un-versioned or non-reproducible state mutations.
- **Single Source of Truth**: The `ExecutionPlan` / `CompiledExecutionPlan` holds the canonical decisions (backend, node, model, model version, strategy, KV decision, timeouts, fencing tokens, lease epochs) and is consumed without re-interpretation.
- **Zero-Trust Telemetry & Real Observations**: No hardcoded operational defaults ("A100", "us-east-1", "v1", "actual_cost = predicted_cost"). Missing or unknown runtime values are explicitly `Unknown` / `Unavailable`.
- **Horizontal Scalability**: Control plane state is decoupled behind `DistributedStateStore` boundaries for sharded admission, capacity, registry, KV metadata, and prediction state.

## Core Decision Pipeline
`auth → authz → admission → immutable world-state snapshot → feasibility → prediction → counterfactual evaluation → utility/SLO → confidence → single Decision Firewall → immutable ExecutionPlan → ExecutionCompiler → inference transport → actual execution → validated observation → prediction error → predictor update → decision memory/audit/telemetry`

## Capabilities
| Capability | Status | Description |
|---|---|---|
| End-to-End Pipeline | Verified | 10-stage deterministic execution pipeline with hard SLO enforcement. |
| Deterministic Candidate Ranking | Verified | Stable `CandidateId` ordering; score-based counterfactual selection. |
| Predictive Inference Intelligence | Verified | Online EWMA latency/cost prediction with MAE/RMSE/P50/P90/P99 error tracking. |
| KV Intelligence & Transport | Verified | Telemetry-driven KV reuse, recompute, and cross-node transfer decisions. |
| Tamper-Evident Audit | Verified | Causally linked SHA-256 hash chain audit trail. |
| Admission & Load Shedding | Verified | Token-bucket rate limiting and tenant-isolated concurrency bounds. |
| Scale & Load Testing | Verified | Built-in load harness for high-concurrency worker benchmarks. |

## Quick Start
```bash
cargo build --release
cargo test --all
cargo clippy --all-targets --all-features -- -D warnings
```

## Testing & Verification
- Unit & Property Tests: `cargo test --all` (130+ unit & integration tests passing).
- Linting: `cargo clippy --all-targets --all-features -- -D warnings`
- Formatting: `cargo fmt --check`

## Security
No prompts or secrets are saved in telemetry/trace records. Request IDs are validated to prevent log injection. Tamper-evident audit chain guarantees audit log integrity.

## License
MIT
