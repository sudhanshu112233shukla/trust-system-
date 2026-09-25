# Execution Boundary

`ExecutionCoordinator` is a backend-agnostic handoff between a selected backend
binding and the existing `HealthMonitor`. It executes only through a registered
backend that declared support for the requested model and KV strategy.

Before a health update is recorded, the result must identify the requested
backend, contain observed latency, and be internally consistent: successful
results carry no failure value; unsuccessful results carry a failure value.
Malformed requests and invalid adapter results are rejected without changing
health or circuit-breaker state.

This is a local execution boundary, not a bundled inference provider. A real
vLLM, OpenAI-compatible, or other provider adapter must be supplied and
registered by deployment code. GPU scheduling, cross-model KV transfer, and
multi-instance state coordination are deliberately not claimed here.