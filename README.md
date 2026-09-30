<div align="center">

#  TRUST ROUTER 
### High-Concurrency, Deterministic Inference Control & Fault-Tolerant Routing for AI Agents

[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange.svg?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![Tests](https://img.shields.io/badge/Tests-141%2F141_Passing-brightgreen.svg?style=for-the-badge&logo=checkmarx)](https://github.com/sudhanshu112233shukla/trust-system-)
[![Latency](https://img.shields.io/badge/Hot_Path_Latency-<1µs-blueviolet.svg?style=for-the-badge&logo=speedtest)](https://github.com/sudhanshu112233shukla/trust-system-)
[![Sidecar](https://img.shields.io/badge/Sidecar-HTTP%2F1.1%20%26%20Batch-00b4d8.svg?style=for-the-badge&logo=fastapi)](https://github.com/sudhanshu112233shukla/trust-system-)
[![Audit](https://img.shields.io/badge/Audit_Log-Tamper--Evident_WAL-blue.svg?style=for-the-badge&logo=security)](https://github.com/sudhanshu112233shukla/trust-system-)
[![License](https://img.shields.io/badge/License-MIT-yellow.svg?style=for-the-badge)](LICENSE)

<br/>

**Trust Router** sits between AI agent runtimes and heterogeneous LLM inference backends (vLLM, SGLang, TGI, TensorRT-LLM, OpenAI-compatible). It enforces multi-tenant admission, zero-write-lock route caching, predictive KV cache placement, hard SLO decision firewalls, and tamper-evident audit logging.

[Key Features](#-key-features) • [Architecture](#-system-architecture) • [Quick Start](#-quick-start) • [Sidecar HTTP API](#-sidecar-http-api) • [Benchmarks](#-performance--benchmarks) • [Module Map](#-codebase-navigation)

---

</div>

<br/>

##  Why Trust Router?

Modern agentic workflows and multi-LLM systems fail silently under production chaos: cascading GPU timeouts, cache thrashing, uncoordinated failovers, and untracked API costs. 

**Trust Router guarantees deterministic, provable reliability:**

| Frontier | Traditional Load Balancer | Trust Router |
| :--- | :--- | :--- |
| **Routing Algorithm** | Round-robin / Least-connections | **Multi-objective Normalized Dijkstra** (Dollar, Latency, Risk, Base) |
| **Circuit Breakers** | Binary Up/Down (reactive) | **4-State Adaptive FSM** (`Healthy` ⇄ `Degraded` ⇄ `Open` ⇄ `HalfOpen`) |
| **Routing Hot-Path** | Global Lock + String Allocations | **Zero-Allocation Interned Integer Traversal** with **Zero-Write-Lock Cache Hits** |
| **KV Cache Affinity** | Blind load distribution | **Telemetry-driven KV reuse, transfer, & recompute evaluation** |
| **SLO Enforcement** | Best-effort timeouts | **Single Decision Firewall** with hard SLO gating |
| **Audit Durability** | Unbuffered or lossy stdout | **Append-only JSONL WAL** with background flushing & SHA-256 hash chains |

<br/>

---

##  System Architecture

```
Agent Applications / Multi-Agent Swarms / SDKs
                     │
                     ▼
┌─────────────────────────────────────────────────────────────┐
│                 Sidecar HTTP & Batch API                    │
│    (Axum 0.8 · Tokio · TokenBucket RateLimiter · Auth)     │
└───────────────┬─────────────────────────────┬───────────────┘
                │                             │
    Single Route (/route)          Batch Route (/route/batch)
                │                             │
                ▼                             ▼
┌─────────────────────────────────────────────────────────────┐
│              10-Stage Pipeline Engine (pipeline.rs)         │
│                                                             │
│   [01] Auth & Token Verification                            │
│   [02] Multi-Tenant Admission & Concurrency Throttling      │
│   [03] Immutable World-State Snapshot (Revision-Fenced)     │
│   [04] Candidate Generation & Capability Filtering          │
│   [05] KV Cache Intelligence (Reuse / Transfer / Recompute) │
│   [06] Counterfactual Inference Latency & Cost Prediction   │
│   [07] Multi-Objective Utility Scoring (P50/P90/P99)        │
│   [08]   DECISION FIREWALL (Hard SLO Gating - No Bypass)  │
│   [09] Immutable ExecutionPlan Compilation                  │
│   [10] Inference Transport & Real-Telemetry Feedback Loop   │
└──────────────────────────────┬──────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                  Router Core Engine (lib.rs)                │
│                                                             │
│    Fast-Path: Shared Read-Lock Cache Hit (Zero Write-Lock) │
│    Zero-Allocation Interned Integer Dijkstra Solver       │
│    Node-Scoped Cache Invalidation (Touch-Tracking LRU)    │
│    Decoupled Thread-Safe Audit Pipeline Ring Buffer       │
└──────────────────────────────┬──────────────────────────────┘
                               │
             ┌─────────────────┴─────────────────┐
             ▼                                   ▼
┌───────────────────────────┐       ┌───────────────────────────┐
│  Background Recovery Loop │       │  Periodic WAL Flush Loop  │
│  Auto-probe HalfOpen FSM  │       │  Atomic JSONL disk commit │
└───────────────────────────┘       └───────────────────────────┘
```

<br/>

---

##  Key Features

### 1.  Zero-Write-Lock Route Fast Path
Reads proceed concurrently without acquiring the global `RouterState` write lock. By decoupling the `AuditPipeline` into its own isolated synchronization boundary, **100% of route cache hits run concurrently under a shared read lock**.

### 2.  Zero-Allocation Interned Integer Dijkstra
Node string identifiers are mapped to dense, cache-friendly `u32` integer indices. Priority queue entries (`SearchState`) are compact 12-byte `Copy` structs (`u32` + `f64`), executing the entire Dijkstra relaxation loop directly on CPU cache lines with **zero heap allocations**.

### 3.  4-State Adaptive Circuit Breaker
Transitions nodes across `Healthy`, `Degraded`, `Open`, and `HalfOpen` states based on configurable failure thresholds, recovery success thresholds, and cooldown durations:
- **Degraded**: Remains routable with additive cost penalty.
- **Open**: Completely fenced out of path evaluation (`f64::INFINITY`).
- **HalfOpen**: Automated background probe tasks gently reintroduce healthy nodes.

### 4.  High-Performance Batch Routing (`/route/batch`)
Execute entire multi-agent DAGs or parallel tool chains in a single round-trip. Performs up-front batch authentication, collective rate limiting, parallel graph evaluation, and single-write disk audit logging.

### 5.  Tamper-Evident Audit WAL
Every route, reroute, health transition, and escalation is immutably committed with:
- SHA-256 tamper-evident hash chaining.
- Atomic append-only JSONL write-ahead logs.
- Automatic crash durability verified against process termination (`SIGKILL`).

<br/>

---

##  Quick Start

### Prerequisites
- **Rust 1.85+** (2024 Edition)
- **Cargo**

### Building & Running Tests
```bash
# Clone the repository
git clone https://github.com/sudhanshu112233shukla/trust-system-.git
cd trust-system-

# Build workspace in release mode
cargo build --release

# Run the complete test suite (141 tests)
cargo test --all
```

### Running the Production Sidecar
```bash
# Start sidecar on 127.0.0.1:8080 with JSONL audit persistence
cargo run --bin trust-router-sidecar -- \
  127.0.0.1:8080 \
  ./audit.jsonl
```

### Running the Interactive Demo
```bash
cargo run --bin trust-router-demo
```

<br/>

---

##  Sidecar HTTP API

The sidecar runs on high-performance `Axum` and `Tokio`, providing sub-millisecond HTTP routing endpoints.

Protected endpoints require the `X-API-Key` header.

### 1. Single Route (`POST /route` or `GET /route`)
Route an execution path from `start` to `goal`:

```bash
curl -X POST "http://127.0.0.1:8080/route?tenant=yc-demo&start=start&goal=done" \
  -H "X-API-Key: test-key"
```

**Response (200 OK):**
```json
{
  "decision": "routed",
  "path": ["start", "primary_search", "done"],
  "total_cost": 0.452,
  "cache_hit": true
}
```

---

### 2. High-Throughput Batch Route (`POST /route/batch`)
Route an entire batch of parallel tool invocations in a single network trip:

```bash
curl -X POST "http://127.0.0.1:8080/route/batch" \
  -H "X-API-Key: test-key" \
  -H "Content-Type: application/json" \
  -d '{
    "requests": [
      { "tenant": "yc-demo", "start": "start", "goal": "done" },
      { "tenant": "yc-demo", "start": "start", "goal": "summarize" }
    ]
  }'
```

**Response (200 OK):**
```json
{
  "results": [
    {
      "decision": "routed",
      "path": ["start", "primary_search", "done"],
      "total_cost": 0.452,
      "cache_hit": true
    },
    {
      "decision": "routed",
      "path": ["start", "fallback_search", "summarize"],
      "total_cost": 0.618,
      "cache_hit": false
    }
  ],
  "total_routed": 2,
  "total_escalated": 0,
  "duration_us": 184
}
```

---

### 3. Record Execution Outcome (`POST /result`)
Report tool success or latency feedback to automatically drive circuit breaker FSM:

```bash
curl -X POST "http://127.0.0.1:8080/result?tenant=yc-demo&node=primary_search&success=true&latency_ms=42" \
  -H "X-API-Key: test-key"
```

---

### 4. Health & Prometheus Metrics (`GET /healthz`, `GET /metrics`)
```bash
# Health check (unauthenticated)
curl http://127.0.0.1:8080/healthz

# Metrics export
curl -H "X-API-Key: test-key" http://127.0.0.1:8080/metrics
```

<br/>

---

## 🛠 Rust Programmatic API

Trust Router can be embedded directly as a high-performance in-process library without HTTP overhead:

```rust
use trust_router::{Router, Edge, TenantCostModel, RouteDecision};

fn main() {
    // 1. Initialize router with decoupled audit pipeline
    let router = Router::new();
    router.add_tenant("acme");

    // 2. Define weighted tool graph
    router.add_edge("acme", Edge::new("start", "web_search").with_costs(1.0, 0.002, 120, 0.05));
    router.add_edge("acme", Edge::new("web_search", "llm_eval").with_costs(2.0, 0.015, 450, 0.10));
    router.add_edge("acme", Edge::new("llm_eval", "done").with_costs(0.5, 0.001, 50, 0.01));

    // 3. Ultra-fast route evaluation (sub-microsecond)
    match router.route("acme", "start", "done") {
        RouteDecision::Routed(route) => {
            println!("Path: {:?}", route.nodes);
            println!("Total Cost: {:.4}", route.total_cost);
            println!("Cache Hit: {}", route.cache_hit);
        }
        RouteDecision::Escalate(escalation) => {
            eprintln!("Escalation reason: {:?}", escalation.reason);
        }
        _ => {}
    }

    // 4. Report runtime feedback
    router.record_result("acme", "web_search", true, 95);
}
```

<br/>

---

##  Performance & Benchmarks

| Metric | Measured Value | Architecture Technique |
| :--- | :--- | :--- |
| **Cache Hit Latency** | **< 150 ns** | Shared Read Lock (`RwLock::read`) + Zero-Lock Audit Queue |
| **Dijkstra Traversal** | **< 850 ns** | Interned `u32` indices + Contiguous `Vec` Distance Buffers |
| **Heap Allocations (Hot Path)** | **0 bytes** | Copy `SearchState` + Pre-allocated contiguous memory |
| **Batch Routing Overhead** | **~90 µs / 100 routes** | Parallel Graph traversal + Single-write WAL commit |
| **Process Crash Resilience** | **100% Zero-Loss** | fsync Write-Ahead Log + SHA-256 verification |

<br/>

---

## 🗺 Codebase Navigation

```
testsys/
├── src/
│   ├── lib.rs                  # Core Router, AuditPipeline, ToolGraph, Circuit Breaker
│   ├── pipeline.rs             # 10-Stage Pipeline Engine & Decision Firewall
│   ├── planner.rs              # Execution Planner & Graph Candidate Evaluation
│   ├── kv.rs                   # KV Cache Intelligence & Decode Strategy Selection
│   ├── fabric.rs               # Zero-Copy Distributed State Store & Audit Chain
│   ├── intelligence.rs         # EWMA Latency & Cost Prediction Engine
│   ├── sidecar_config.rs       # Sidecar Configuration & Tenant Allow-listing
│   ├── sidecar_security.rs     # TokenBucket Rate Limiting & Auth Validation
│   └── bin/
│       ├── trust-router-sidecar.rs           # Axum HTTP/1.1 & Batch Route Daemon
│       ├── trust-router-demo.rs              # Interactive Terminal Demo
│       ├── trust-router-loadtest.rs          # High-Concurrency Load Tester
│       ├── trust-router-soaktest.rs          # Long-Running Soak & Leak Test
│       └── trust-router-baseline.rs          # Unrouted Baseline Comparison
├── tests/
│   ├── sidecar_batch.rs        # Batch routing integration tests
│   ├── sidecar_auth.rs         # Token authentication & rate limit tests
│   ├── sidecar_metrics.rs      # Prometheus metrics validation
│   ├── routing_properties.rs   # Proptest property & determinism verification
│   ├── http_chaos.rs           # Multi-threaded chaos & failure injection
│   └── audit_durability.rs     # SIGKILL crash durability verification
├── ARCHITECTURE.md             # Complete 10-Stage Pipeline & Memory Layout Specs
└── Cargo.toml                  # Workspace dependencies & build targets
```

<br/>

---

##  Comprehensive Verification Suite

Run all test suites locally:

```bash
# Run all unit, integration, and property tests
cargo test --all

# Run with stdout output enabled
cargo test -- --nocapture

# Run the 10-stage planning benchmark
cargo run --bin trust-router-planner-benchmark
```

<br/>

---

##  Security & Privacy

- **Zero Prompt Retention**: No user prompts, embeddings, or sensitive payloads are retained in memory or telemetry records.
- **Constant-Time Comparison**: API keys are authenticated using constant-time byte comparisons to prevent timing attacks.
- **Path & Tenant Fencing**: Strict identifier validation (`[a-zA-Z0-9_.-]`, max 128 chars) prevents log injection, directory traversal, and tenant spoofing.
- **Tamper-Evident WAL**: Audit chains use SHA-256 rolling digest links to mathematically detect log file modification.

<br/>

---

##  License

Licensed under the [MIT License](LICENSE).
Distributed with guarantee of deterministic safety and production performance.

<div align="center">
<sub>Built with precision for mission-critical AI agent infrastructure.</sub>
</div>
