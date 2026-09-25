# Trust Router
Deterministic control-plane decisions for AI-agent and inference workflows.

## What It Is
Trust Router sits above inference execution engines. It observes inference outcomes, applies policy, and evaluates deterministic execution bounds before compiling safe execution plans. It does not replace serving layers (like vLLM) but decides how and when they are used.

## The Problem
Standard agent architectures couple model execution with physical capacity/KV intelligence routing. Trust Router isolates decision-making.

## Design Principles
- Deterministic safety
- Monotonic state revisions
- Zero-trust execution telemetry

## Architecture
```text
Client / SDK
|
API + Auth
|
Admission Control
|
Trust Intelligence
|
Decision Firewall
|
Execution Compiler
|
Inference Fabric
|
Backend / GPU / KV
|
Observation
|
Prediction Error
|
Decision Memory
```

## Core Decision Pipeline
Observe → Build World State → Predict → Generate Counterfactuals → Apply SLO → Decision Firewall → Compile Execution Plan → Execute → Observe Actual Result → Measure Prediction Error → Improve Future Decisions.

## Capabilities
| Capability | Status | Description |
|---|---|---|
| Safety boundary | Implemented | Prevents SLO violations. |
| Predictive Inference Intelligence | Planning-only | Basic online constraints structure. |
| KV Intelligence | Experimental | Compatibility tracking and routing logic. |
| Execution Fabric | Adapter boundary | Transport abstractions for engines. |
| Admission and Backpressure | Implemented | Concurrency limits and shedding. |

## Quick Start
```bash
cargo build --release
cargo test
```

## Configuration
Requires valid cost weights, admission limits, and tenant capabilities.

## Testing
`cargo test` runs all property, unit, and deterministic logic verification tests.

## Security
No prompts or secrets are saved in telemetry/trace records.

## Limitations
Distributed state is mocked via in-memory interfaces. Not a drop-in replacement for vLLM. No automatic self-learning logic.

## License
MIT
