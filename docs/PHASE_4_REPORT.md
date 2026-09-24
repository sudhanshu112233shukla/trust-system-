# Phase 4 Report: KV Intelligence Foundation

Phase 4 adds a backend-agnostic model registry, declared model KV capabilities, and a version-aware compatibility registry. `ExecutionPlan` now carries additive KV metadata while existing `/plan` behavior remains normal-prefill by default.

A KV strategy is selected only when its target model declares support. Cross-model KV transfer is selected only when a registered compatibility record is explicitly supported and contains a mapping version, positive sample count, transfer latency measurements, quality retention, confidence, and failure-rate measurements meeting the configured conservative policy. Unknown or insufficient information produces normal prefill with a structured fallback reason; no compatibility or performance values are fabricated.

This phase does not execute inference, transfer KV state, add an external backend, or integrate OneTriangle. The model and compatibility registries are process-local planning metadata and are not exposed through the sidecar API yet.

Verification: existing routing/planner tests plus focused capability-selection and unknown-measurement fallback tests. Re-run `cargo test`, `cargo clippy --all-targets -- -D warnings`, and the planner benchmark before relying on deployment performance numbers.