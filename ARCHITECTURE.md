# Trust Router Architecture

```text
agent / SDK
    |
    v
sidecar HTTP API
    |
    v
router core
    |
    +-- graph engine
    +-- health tracker
    +-- path cache
    +-- audit writer
    |
    v
background recovery loop
```

## Sidecar HTTP API

The sidecar is the process boundary for agents and SDKs. It exposes route, result, metrics, and recovery endpoints while keeping the routing engine embedded and deterministic. This keeps customer agent code small: call `/route`, execute the selected tool path, and report outcomes with `/result`.

## Router Core

The core owns the tenant graph, health state, route cache, and audit event stream. It is separated from the sidecar so the same deterministic behavior can be tested in-process, served over HTTP, or later reused behind gRPC/SDK integrations.

## Graph Engine

The graph engine chooses the lowest-cost feasible route through tenant-owned tool nodes. Cost is normalized so dollar spend, latency, business risk, and base operational cost can be compared without hardcoding one unit as dominant.

## Health Tracker

The health tracker turns tool outcomes into circuit-breaker states. `Degraded` and `HalfOpen` remain routable with penalties, while `Open` is excluded. This gives the router a deterministic way to avoid clearly broken tools without treating every slowdown as a full outage.

## Path Cache

The cache stores recent route decisions. Invalidation is node-scoped so a health change only evicts paths that touched the changed node. That keeps route latency low while avoiding stale paths through failed tools.

## Audit Writer

The audit writer persists route and escalation events as JSONL. This is the durable trust record: it explains what the router decided, when it decided it, and whether it rerouted or escalated.

## Background Recovery Loop

The recovery loop moves open nodes into `HalfOpen` after cooldown so they can be probed safely. It is separate from routing because recovery is time-driven, while routing is request-driven.
