# Architecture

```text
agent / SDK
  |
  v
Axum sidecar
  |-- authentication + tenant authorization + rate limiting
  |-- API validation + request IDs + structured errors
  |
  v
Planner
  |-- request analyzer
  |-- system snapshot
  |-- candidate generation
  |-- feasibility and score validation
  |-- selection
  |
  +--> ExecutionPlan --> Python / TypeScript execution adapters
  |
  +--> bounded recovery plan --> explicit LLM or operator escalation
  |
  v
Router core
  |-- tenant graph engine
  |-- health / circuit breaker
  |-- bounded LRU route cache
  |-- bounded in-memory audit history
  |-- durable JSONL audit writer
  |
  v
async result monitor and recovery loop
```

## Boundaries

The router core owns correctness: feasible paths, health penalties, circuit breaker exclusion, cache invalidation, and audit decisions. The planner owns only conversion from a validated request to a bounded execution or recovery contract. It deliberately has no side effects beyond invoking the router core, so an adapter can execute the plan with a local tool registry, LangChain, MCP, or a future backend.

The sidecar owns network concerns: authentication, authorization, input validation, rate limiting, metrics, and durable audit persistence before a routing or planning response. It does not execute tools.

## State and Deployment Model

One process has one in-memory router state. Its route cache, health, circuit-breaker state, metrics, and planner inputs reset on restart. Audit JSONL persists wherever its file path is mounted. Multiple sidecars are independent and unsupported as a coordinated cluster.