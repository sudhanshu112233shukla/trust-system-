# Trust Router Architecture

## System Overview

```
Agent / SDK (Python · TypeScript)
            │
            ▼
   Sidecar HTTP API
   (sidecar_config.rs + sidecar_security.rs)
            │  API-key auth, config hot-reload
            ▼
   ┌─────────────────────────────────────────────────────────┐
   │              PipelineEngine  (pipeline.rs)              │
   │                                                         │
   │  Stage 1  ── Auth / API-key validation                  │
   │  Stage 2  ── Admission control (token-bucket, per-      │
   │              tenant concurrency)                        │
   │  Stage 3  ── Immutable world-state snapshot             │
   │              (WorldStateManager, revision-fenced)       │
   │  Stage 4  ── Candidate generation                       │
   │              (SystemPlanner: capacity × backend caps)   │
   │  Stage 5  ── KV intelligence decision                   │
   │              (KvIntelligence: reuse/transfer/recompute) │
   │  Stage 6  ── Counterfactual prediction & ranking        │
   │              (CounterfactualEngine + PredictionEngine)  │
   │  Stage 7  ── Utility scoring + SLO gating               │
   │              (ObjectiveWeights + UtilityConfig)         │
   │  Stage 8  ── Decision Firewall (hard SLO enforcement,   │
   │              single choke-point, no bypass)             │
   │  Stage 9  ── ExecutionPlan compile                      │
   │              (ExecutionCompiler → CompiledExecutionPlan)│
   │  Stage 10 ── Inference transport + feedback loop        │
   │              (InferenceTransport → real observations →  │
   │               PredictionEngine EWMA update)             │
   └─────────────────────────────────────────────────────────┘
            │
            ▼
   Router Core  (lib.rs)
   ┌──────────────────────────────────┐
   │  ToolGraph    Dijkstra shortest- │
   │               path, health-      │
   │               penalised weights  │
   │                                  │
   │  HealthTracker  4-state FSM      │
   │  Healthy → Degraded → Open       │
   │          → HalfOpen → Healthy    │
   │                                  │
   │  RouteCache   LRU 1 024 entries, │
   │               node-scoped        │
   │               invalidation       │
   │                                  │
   │  AuditLog     VecDeque 10 000    │
   │               events (JSONL)     │
   └──────────────────────────────────┘
            │
            ▼
   Background Loops  (tokio::spawn)
   ┌──────────────────────────────────┐
   │  Recovery Loop   Open → HalfOpen │
   │  after cooldown (spawn_blocking  │
   │  so runtime is never starved)    │
   │                                  │
   │  Audit Flush Loop  periodic      │
   │  JSONL flush to disk guarding    │
   │  against in-flight event loss    │
   └──────────────────────────────────┘
```

---

## Sidecar HTTP API

The sidecar is the process boundary for agents and SDKs. It exposes route, result,
metrics, and recovery endpoints while keeping the routing engine embedded and
deterministic. API-key validation is handled by `sidecar_security.rs`; configuration
(including hot-reload) by `sidecar_config.rs`.

---

## PipelineEngine

The ten-stage pipeline (`pipeline.rs`) is the authoritative inference-decision path.
Each stage either rejects the request with an explicit, structured reason or advances
an immutable request descriptor to the next gate. No stage may bypass the Decision
Firewall. Identical inputs always produce identical `CompiledExecutionPlan` outputs.

---

## Router Core

The core owns the tenant graph, health state, route cache, and audit event stream. It
is intentionally decoupled from the sidecar so the same deterministic routing logic can
be tested in-process, served over HTTP, or reused behind gRPC / SDK integrations.

**Lock strategy**: `route()` uses a *read* lock for cache hits — allowing concurrent
route calls to proceed in parallel — and upgrades to a *write* lock only on a cache
miss or when recording an escalation event. This ensures the common case is not
serialised.

---

## Graph Engine

The graph engine chooses the lowest-cost feasible route through tenant-owned tool
nodes. Cost is a four-weight linear combination (dollar spend, latency, business risk,
base operational cost) normalised to `[0, 1]` against per-tenant ceilings. Open
circuit-breaker nodes receive an `∞` penalty and are excluded from Dijkstra.

---

## Health Tracker

The health tracker turns tool outcomes into circuit-breaker states. `Degraded` and
`HalfOpen` remain routable with additive penalties; `Open` is excluded. The EWMA
success rate (`α = 0.1`) and rolling P99 latency (128-sample window) feed the penalty
calculation so the router degrades gracefully before full exclusion.

---

## Path Cache

The cache stores recent route decisions in an LRU structure (capacity: 1 024 entries).
Invalidation is **node-scoped**: a health-state change evicts only the paths that
touched the changed node. Paths unaffected by the change remain cached, keeping route
latency low without serving stale paths through failed tools.

---

## Audit System

Audit events are kept in an in-memory ring buffer (10 000 events) and serialised as
JSONL. `AuditChain` in `fabric.rs` adds a SHA-256 hash chain across all pipeline-level
decisions, making the audit trail tamper-evident without an external database.

The **Audit Flush Loop** (`spawn_audit_flush_loop` / `spawn_default_audit_flush_loop`)
periodically writes the ring buffer to disk so events are durable across process
restarts without requiring callers to flush on every request.

---

## Background Recovery Loop

The recovery loop moves open nodes into `HalfOpen` after the configured cooldown. It is
time-driven (separate from the request-driven routing path) and offloads the write-lock
work to a blocking thread via `tokio::task::spawn_blocking` so the async runtime is
never starved under large node counts.

---

## Distributed State Boundary

`DistributedStateStore` (in `fabric.rs`) abstracts sharded cluster state (capacity
snapshots, backend registry). `LocalStateStore` is the single-process implementation;
fields are wrapped in `Arc<RwLock<_>>` so fetches are O(1) pointer-bump clones rather
than full data copies.
