//! Production inference fabric: adapters, admission, backpressure, KV decisions,
//! node lifecycle, distributed state, execution compiler, and audit integrity.
//!
//! This module provides the adapter boundary between the deterministic control
//! plane and physical inference backends. It never executes inference directly;
//! it compiles validated execution plans and provides trait boundaries for
//! provider-specific transport implementations.

use crate::intelligence::CounterfactualEstimate;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

// =========================================================================
// Error types
// =========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FabricError {
    Timeout,
    CapabilityMismatch,
    AdmissionRejected(&'static str),
    StaleLease,
    QueueFull,
    BackendUnavailable,
    IdentityMismatch,
    MalformedResponse,
    KvTransferFailed,
    StaleReadiness,
}

impl std::fmt::Display for FabricError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for FabricError {}

// =========================================================================
// InferenceTransport — provider-independent execution adapter
// =========================================================================

/// Transport-level request handed to an inference adapter.
#[derive(Debug, Clone)]
pub struct TransportRequest {
    pub request_id: String,
    pub model: String,
    pub model_version: String,
    pub backend_id: String,
    pub timeout: Duration,
}

/// Transport-level response from an inference adapter.
#[derive(Debug, Clone)]
pub struct TransportResponse {
    pub request_id: String,
    pub backend_id: String,
    pub success: bool,
    pub latency_ms: u64,
    pub failure_class: Option<String>,
}

/// Abstraction over vLLM, OpenAI-compatible, or custom inference endpoints.
pub trait InferenceTransport: Send + Sync {
    fn execute(&self, request: &TransportRequest) -> Result<TransportResponse, FabricError>;
    fn health(&self) -> bool;
    fn capabilities(&self) -> Vec<String>;
    fn cancel(&self, request_id: &str);
}

/// Deterministic fake transport for unit tests. Never makes network calls.
pub struct DeterministicTransport {
    pub healthy: bool,
    pub latency_ms: u64,
    pub capabilities: Vec<String>,
}

impl InferenceTransport for DeterministicTransport {
    fn execute(&self, request: &TransportRequest) -> Result<TransportResponse, FabricError> {
        if !self.healthy {
            return Err(FabricError::BackendUnavailable);
        }
        Ok(TransportResponse {
            request_id: request.request_id.clone(),
            backend_id: request.backend_id.clone(),
            success: true,
            latency_ms: self.latency_ms,
            failure_class: None,
        })
    }
    fn health(&self) -> bool {
        self.healthy
    }
    fn capabilities(&self) -> Vec<String> {
        self.capabilities.clone()
    }
    fn cancel(&self, _request_id: &str) {}
}

// =========================================================================
// KvTransport — provider-independent KV transfer boundary
// =========================================================================

pub trait KvTransport: Send + Sync {
    fn can_transfer(&self, source: &str, dest: &str) -> bool;
    fn estimate_transfer(&self, size_mb: u64) -> Duration;
    fn transfer(&self, source: &str, dest: &str) -> Result<(), FabricError>;
    fn verify(&self, dest: &str) -> bool;
    fn release(&self, node: &str);
}

/// Deterministic fake KV transport for unit tests.
pub struct DeterministicKvTransport {
    pub allow_transfer: bool,
    pub transfer_latency_per_mb: Duration,
}

impl KvTransport for DeterministicKvTransport {
    fn can_transfer(&self, _source: &str, _dest: &str) -> bool {
        self.allow_transfer
    }
    fn estimate_transfer(&self, size_mb: u64) -> Duration {
        self.transfer_latency_per_mb * size_mb as u32
    }
    fn transfer(&self, _source: &str, _dest: &str) -> Result<(), FabricError> {
        if self.allow_transfer {
            Ok(())
        } else {
            Err(FabricError::KvTransferFailed)
        }
    }
    fn verify(&self, _dest: &str) -> bool {
        self.allow_transfer
    }
    fn release(&self, _node: &str) {}
}

// =========================================================================
// KV decision engine
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvAction {
    Reuse,
    Transfer,
    Keep,
    Evict,
    Migrate,
    Recompute,
}

#[derive(Debug, Clone)]
pub struct KvDecisionInput {
    pub compatible: bool,
    pub kv_size_mb: u64,
    pub transfer_latency_ms: u64,
    pub network_cost: f64,
    pub recomputation_ms: u64,
    pub expected_reuse: f64,
    pub failure_risk: f64,
    pub confidence: f64,
}

impl KvDecisionInput {
    /// Deterministic KV action selection. Unknown compatibility never transfers.
    pub fn decide(&self) -> (KvAction, &'static str) {
        if !self.compatible {
            return (KvAction::Recompute, "INCOMPATIBLE_KV");
        }
        if self.confidence < 0.5 {
            return (KvAction::Recompute, "LOW_CONFIDENCE");
        }
        if self.failure_risk > 0.3 {
            return (KvAction::Recompute, "HIGH_FAILURE_RISK");
        }
        // Transfer only when benefit > cost + risk
        let transfer_cost = self.transfer_latency_ms as f64 + self.network_cost;
        let recompute_cost = self.recomputation_ms as f64;
        if transfer_cost + (self.failure_risk * recompute_cost) <= recompute_cost {
            (KvAction::Transfer, "TRANSFER_BENEFICIAL")
        } else {
            (KvAction::Recompute, "TRANSFER_COST_EXCEEDS_BENEFIT")
        }
    }
}

// =========================================================================
// NodeTelemetryProvider — GPU/compute metrics boundary
// =========================================================================

#[derive(Debug, Clone)]
pub struct NodeTelemetry {
    pub timestamp: SystemTime,
    pub node_id: String,
    pub revision: u64,
    pub provider_version: String,
    pub gpu_utilization: f64,
    pub gpu_memory_used_mb: u64,
    pub gpu_memory_total_mb: u64,
    pub compute_availability: f64,
    pub queue_depth: u32,
    pub active_requests: u32,
    pub model_residency: Vec<String>,
    pub kv_utilization: f64,
}

impl NodeTelemetry {
    pub fn is_stale(&self, max_age: Duration) -> bool {
        self.timestamp
            .elapsed()
            .map_or(true, |elapsed| elapsed > max_age)
    }

    pub fn validate(&self) -> Result<(), FabricError> {
        if self.node_id.is_empty() || self.node_id.len() > 128 {
            return Err(FabricError::MalformedResponse);
        }
        if self.gpu_utilization.is_nan() || !(0.0..=1.0).contains(&self.gpu_utilization) {
            return Err(FabricError::MalformedResponse);
        }
        if self.kv_utilization.is_nan() || !(0.0..=1.0).contains(&self.kv_utilization) {
            return Err(FabricError::MalformedResponse);
        }
        Ok(())
    }
}

pub trait NodeTelemetryProvider: Send + Sync {
    fn collect(&self, node_id: &str) -> Result<NodeTelemetry, FabricError>;
}

/// Deterministic test telemetry provider.
pub struct DeterministicTelemetryProvider {
    pub gpu_utilization: f64,
    pub queue_depth: u32,
}

impl NodeTelemetryProvider for DeterministicTelemetryProvider {
    fn collect(&self, node_id: &str) -> Result<NodeTelemetry, FabricError> {
        Ok(NodeTelemetry {
            timestamp: SystemTime::now(),
            node_id: node_id.to_string(),
            revision: 1,
            provider_version: "test-v1".into(),
            gpu_utilization: self.gpu_utilization,
            gpu_memory_used_mb: 4000,
            gpu_memory_total_mb: 8000,
            compute_availability: 1.0 - self.gpu_utilization,
            queue_depth: self.queue_depth,
            active_requests: 0,
            model_residency: vec![],
            kv_utilization: 0.0,
        })
    }
}

// =========================================================================
// Admission control — deterministic load shedding
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDecision {
    Accept,
    Queue,
    Defer,
    Shed,
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionRejection {
    CapacityExhausted,
    QueueFull,
    TenantQuotaExceeded,
    ConcurrencyLimit,
    TimeoutBudgetInsufficient,
    RetryBudgetExhausted,
    SloInfeasible,
}

pub struct AdmissionController {
    global_concurrency_limit: u32,
    max_queue_depth: u32,
    active_requests: AtomicU32,
    queue_depth: AtomicU32,
}

impl AdmissionController {
    pub fn new(global_concurrency_limit: u32, max_queue_depth: u32) -> Self {
        Self {
            global_concurrency_limit,
            max_queue_depth,
            active_requests: AtomicU32::new(0),
            queue_depth: AtomicU32::new(0),
        }
    }

    pub fn evaluate(&self) -> (AdmissionDecision, Option<AdmissionRejection>) {
        let active = self.active_requests.load(Ordering::Relaxed);
        if active < self.global_concurrency_limit {
            self.active_requests.fetch_add(1, Ordering::Relaxed);
            return (AdmissionDecision::Accept, None);
        }
        let queue = self.queue_depth.load(Ordering::Relaxed);
        if queue < self.max_queue_depth {
            self.queue_depth.fetch_add(1, Ordering::Relaxed);
            return (AdmissionDecision::Queue, None);
        }
        (AdmissionDecision::Shed, Some(AdmissionRejection::QueueFull))
    }

    pub fn complete(&self) {
        let prev = self.active_requests.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(prev > 0, "completed more requests than accepted");
    }

    pub fn dequeue(&self) {
        let prev = self.queue_depth.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(prev > 0, "dequeued more than queued");
    }

    pub fn active_requests(&self) -> u32 {
        self.active_requests.load(Ordering::Relaxed)
    }

    pub fn queue_depth(&self) -> u32 {
        self.queue_depth.load(Ordering::Relaxed)
    }
}

// =========================================================================
// Bounded backpressure queue
// =========================================================================

pub struct BoundedQueue<T> {
    items: RwLock<VecDeque<T>>,
    capacity: usize,
}

impl<T> BoundedQueue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: RwLock::new(VecDeque::with_capacity(capacity.min(10_000))),
            capacity,
        }
    }

    pub fn push(&self, item: T) -> Result<(), FabricError> {
        let mut items = self.items.write().expect("queue lock poisoned");
        if items.len() >= self.capacity {
            return Err(FabricError::QueueFull);
        }
        items.push_back(item);
        Ok(())
    }

    pub fn pop(&self) -> Option<T> {
        let mut items = self.items.write().expect("queue lock poisoned");
        items.pop_front()
    }

    pub fn len(&self) -> usize {
        self.items.read().expect("queue lock poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// =========================================================================
// Backend/node lifecycle state machine
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeLifecycleState {
    Healthy,
    Degraded,
    Suspect,
    Draining,
    Failed,
    Recovering,
}

#[derive(Debug, Clone)]
pub struct NodeLifecycleTransition {
    pub from: NodeLifecycleState,
    pub to: NodeLifecycleState,
    pub reason: String,
    pub timestamp: SystemTime,
    pub revision: u64,
}

pub struct NodeLifecycle {
    state: NodeLifecycleState,
    consecutive_failures: u32,
    consecutive_successes: u32,
    transitions: VecDeque<NodeLifecycleTransition>,
    max_transitions: usize,
    revision: u64,
}

impl NodeLifecycle {
    pub fn new(max_transitions: usize) -> Self {
        Self {
            state: NodeLifecycleState::Healthy,
            consecutive_failures: 0,
            consecutive_successes: 0,
            transitions: VecDeque::new(),
            max_transitions: max_transitions.clamp(1, 1000),
            revision: 0,
        }
    }

    pub fn state(&self) -> NodeLifecycleState {
        self.state
    }

    pub fn record_success(&mut self) {
        self.consecutive_successes += 1;
        self.consecutive_failures = 0;
        match self.state {
            NodeLifecycleState::Recovering if self.consecutive_successes >= 3 => {
                self.transition(NodeLifecycleState::Healthy, "recovery_complete");
            }
            NodeLifecycleState::Degraded if self.consecutive_successes >= 2 => {
                self.transition(NodeLifecycleState::Healthy, "degraded_recovery");
            }
            NodeLifecycleState::Suspect if self.consecutive_successes >= 1 => {
                self.transition(NodeLifecycleState::Degraded, "suspect_improving");
            }
            _ => {}
        }
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.consecutive_successes = 0;
        match self.consecutive_failures {
            f if f >= 5 && self.state != NodeLifecycleState::Failed => {
                self.transition(NodeLifecycleState::Failed, "failure_threshold");
            }
            f if f >= 3 && self.state == NodeLifecycleState::Healthy => {
                self.transition(NodeLifecycleState::Suspect, "repeated_failures");
            }
            f if f >= 2
                && matches!(
                    self.state,
                    NodeLifecycleState::Healthy | NodeLifecycleState::Degraded
                ) =>
            {
                self.transition(NodeLifecycleState::Degraded, "failures_increasing");
            }
            _ => {}
        }
    }

    pub fn drain(&mut self) {
        if self.state != NodeLifecycleState::Draining {
            self.transition(NodeLifecycleState::Draining, "manual_drain");
        }
    }

    pub fn begin_recovery(&mut self) {
        if self.state == NodeLifecycleState::Failed {
            self.transition(NodeLifecycleState::Recovering, "recovery_initiated");
            self.consecutive_failures = 0;
            self.consecutive_successes = 0;
        }
    }

    pub fn transitions(&self) -> &VecDeque<NodeLifecycleTransition> {
        &self.transitions
    }

    fn transition(&mut self, to: NodeLifecycleState, reason: &str) {
        self.revision += 1;
        let record = NodeLifecycleTransition {
            from: self.state,
            to,
            reason: reason.to_string(),
            timestamp: SystemTime::now(),
            revision: self.revision,
        };
        if self.transitions.len() >= self.max_transitions {
            self.transitions.pop_front();
        }
        self.transitions.push_back(record);
        self.state = to;
    }
}

// =========================================================================
// Distributed state: Epoch, Lease, FencingToken
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Epoch(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FencingToken(pub u64);

#[derive(Debug, Clone)]
pub struct Lease {
    pub epoch: Epoch,
    pub owner: String,
    pub expires_at: SystemTime,
}

impl Lease {
    pub fn is_valid(&self, current_time: SystemTime) -> bool {
        current_time < self.expires_at
    }
}

pub trait LeaseStore: Send + Sync {
    fn acquire(&mut self, owner: &str, ttl: Duration) -> Option<Lease>;
    fn verify(&self, epoch: Epoch) -> bool;
    fn current_epoch(&self) -> Epoch;
}

pub struct InMemoryLeaseStore {
    current_lease: Option<Lease>,
    epoch: Epoch,
}

impl InMemoryLeaseStore {
    pub fn new() -> Self {
        Self {
            current_lease: None,
            epoch: Epoch(0),
        }
    }
}

impl LeaseStore for InMemoryLeaseStore {
    fn acquire(&mut self, owner: &str, ttl: Duration) -> Option<Lease> {
        let now = SystemTime::now();
        if let Some(lease) = &self.current_lease {
            if lease.is_valid(now) && lease.owner != owner {
                return None;
            }
        }
        self.epoch.0 += 1;
        let new_lease = Lease {
            epoch: self.epoch,
            owner: owner.to_string(),
            expires_at: now + ttl,
        };
        self.current_lease = Some(new_lease.clone());
        Some(new_lease)
    }

    fn verify(&self, epoch: Epoch) -> bool {
        self.current_lease
            .as_ref()
            .is_some_and(|l| l.epoch == epoch && l.is_valid(SystemTime::now()))
    }

    fn current_epoch(&self) -> Epoch {
        self.epoch
    }
}

/// Persistent state store boundary (in-memory for now, pluggable later).
pub trait StateStore: Send + Sync {
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn put(&mut self, key: &str, value: Vec<u8>, revision: u64) -> Result<(), FabricError>;
    fn compare_and_swap(
        &mut self,
        key: &str,
        expected_revision: u64,
        value: Vec<u8>,
    ) -> Result<(), FabricError>;
    fn revision(&self, key: &str) -> Option<u64>;
}

// =========================================================================
// Execution phases — disaggregated inference support
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPhase {
    Prefill,
    Decode,
    PrefillDecode,
}

// =========================================================================
// Backend readiness — runtime vs declared capability
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendReadiness {
    Declared,
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct RuntimeBackendState {
    pub backend_id: String,
    pub readiness: BackendReadiness,
    pub model_loaded: bool,
    pub model_version: String,
    pub health_ok: bool,
    pub last_checked: SystemTime,
}

impl RuntimeBackendState {
    pub fn is_selectable(&self) -> bool {
        matches!(self.readiness, BackendReadiness::Ready)
            && self.model_loaded
            && self.health_ok
    }

    pub fn is_stale(&self, max_age: Duration) -> bool {
        self.last_checked
            .elapsed()
            .map_or(true, |elapsed| elapsed > max_age)
    }
}

// =========================================================================
// Version-aware lifecycle
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionStage {
    Candidate,
    Shadow,
    Canary,
    Active,
    Deprecated,
    RolledBack,
}

#[derive(Debug, Clone)]
pub struct VersionedArtifact {
    pub name: String,
    pub version: String,
    pub stage: VersionStage,
}

// =========================================================================
// Shadow evaluation
// =========================================================================

#[derive(Debug, Clone)]
pub struct ShadowEvaluation {
    pub primary_backend: String,
    pub shadow_backend: String,
    pub primary_score: f64,
    pub shadow_score: f64,
    pub difference: f64,
    pub confidence: f64,
}

// =========================================================================
// Canary stages
// =========================================================================

#[derive(Debug, Clone)]
pub struct CanaryConfig {
    pub stages: Vec<f64>,
    pub max_error_rate: f64,
    pub max_p99_latency_ms: u64,
    pub max_cost_regression: f64,
    pub min_samples_per_stage: u64,
}

impl Default for CanaryConfig {
    fn default() -> Self {
        Self {
            stages: vec![0.01, 0.05, 0.25, 0.50, 1.0],
            max_error_rate: 0.05,
            max_p99_latency_ms: 500,
            max_cost_regression: 0.1,
            min_samples_per_stage: 100,
        }
    }
}

impl CanaryConfig {
    pub fn should_auto_stop(
        &self,
        error_rate: f64,
        p99_latency_ms: u64,
        cost_regression: f64,
    ) -> Option<&'static str> {
        if error_rate > self.max_error_rate {
            return Some("ERROR_RATE_EXCEEDED");
        }
        if p99_latency_ms > self.max_p99_latency_ms {
            return Some("P99_LATENCY_EXCEEDED");
        }
        if cost_regression > self.max_cost_regression {
            return Some("COST_REGRESSION");
        }
        None
    }

    pub fn can_promote(&self, samples: u64, current_stage_index: usize) -> bool {
        samples >= self.min_samples_per_stage
            && current_stage_index < self.stages.len().saturating_sub(1)
    }
}

// =========================================================================
// Execution compiler
// =========================================================================

#[derive(Debug, Clone)]
pub struct CompiledExecutionPlan {
    pub schema_version: u8,
    pub model: String,
    pub model_version: String,
    pub backend: String,
    pub node: String,
    pub execution_phase: ExecutionPhase,
    pub strategy: String,
    pub timeout: Duration,
    pub retry_budget: u32,
    pub kv_action: KvAction,
    pub kv_reason: String,
    pub quality_floor: f64,
    pub world_revision: u64,
    pub lease_epoch: u64,
    pub fencing_token: u64,
}

pub struct ExecutionCompiler;

impl ExecutionCompiler {
    pub fn compile(
        candidate: &CounterfactualEstimate,
        kv_decision: (KvAction, &str),
        quality_floor: f64,
        world_revision: u64,
        lease_epoch: u64,
        fencing_token: u64,
    ) -> CompiledExecutionPlan {
        CompiledExecutionPlan {
            schema_version: 2,
            model: candidate.model.clone(),
            model_version: "v1".to_string(),
            backend: candidate.backend_id.clone(),
            node: candidate.compute_node_id.clone(),
            execution_phase: ExecutionPhase::PrefillDecode,
            strategy: candidate.strategy.clone(),
            timeout: Duration::from_millis(5000),
            retry_budget: 3,
            kv_action: kv_decision.0,
            kv_reason: kv_decision.1.to_string(),
            quality_floor,
            world_revision,
            lease_epoch,
            fencing_token,
        }
    }
}

// =========================================================================
// Tamper-evident audit chain
// =========================================================================

#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub schema_version: u8,
    pub event_id: String,
    pub previous_hash: String,
    pub current_hash: String,
    pub timestamp: SystemTime,
    pub actor: String,
    pub world_revision: u64,
}

impl AuditRecord {
    pub fn compute_hash(event_id: &str, previous_hash: &str, actor: &str) -> String {
        format!(
            "{:x}",
            md5::compute(format!("{event_id}-{previous_hash}-{actor}"))
        )
    }
}

pub struct AuditChain {
    records: VecDeque<AuditRecord>,
    max_records: usize,
}

impl AuditChain {
    pub fn new(max_records: usize) -> Self {
        Self {
            records: VecDeque::new(),
            max_records: max_records.clamp(1, 100_000),
        }
    }

    pub fn append(&mut self, event_id: String, actor: String, world_revision: u64) {
        let previous_hash = self
            .records
            .back()
            .map_or_else(|| "genesis".to_string(), |r| r.current_hash.clone());
        let current_hash = AuditRecord::compute_hash(&event_id, &previous_hash, &actor);
        let record = AuditRecord {
            schema_version: 1,
            event_id,
            previous_hash,
            current_hash,
            timestamp: SystemTime::now(),
            actor,
            world_revision,
        };
        if self.records.len() >= self.max_records {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    pub fn verify_integrity(&self) -> Result<(), &'static str> {
        let mut iter = self.records.iter().peekable();
        while let Some(record) = iter.next() {
            let expected = AuditRecord::compute_hash(
                &record.event_id,
                &record.previous_hash,
                &record.actor,
            );
            if record.current_hash != expected {
                return Err("TAMPERED_RECORD");
            }
            if let Some(next) = iter.peek() {
                if next.previous_hash != record.current_hash {
                    return Err("BROKEN_CHAIN");
                }
            }
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

// =========================================================================
// Operational metrics (bounded counters)
// =========================================================================

pub struct FabricMetrics {
    pub admission_accepted: AtomicU64,
    pub admission_queued: AtomicU64,
    pub admission_shed: AtomicU64,
    pub executions: AtomicU64,
    pub execution_failures: AtomicU64,
    pub kv_transfers: AtomicU64,
    pub kv_recomputes: AtomicU64,
    pub stale_readiness_rejections: AtomicU64,
    pub firewall_approvals: AtomicU64,
    pub firewall_rejections: AtomicU64,
    pub low_confidence_decisions: AtomicU64,
    pub canary_auto_stops: AtomicU64,
}

impl FabricMetrics {
    pub fn new() -> Self {
        Self {
            admission_accepted: AtomicU64::new(0),
            admission_queued: AtomicU64::new(0),
            admission_shed: AtomicU64::new(0),
            executions: AtomicU64::new(0),
            execution_failures: AtomicU64::new(0),
            kv_transfers: AtomicU64::new(0),
            kv_recomputes: AtomicU64::new(0),
            stale_readiness_rejections: AtomicU64::new(0),
            firewall_approvals: AtomicU64::new(0),
            firewall_rejections: AtomicU64::new(0),
            low_confidence_decisions: AtomicU64::new(0),
            canary_auto_stops: AtomicU64::new(0),
        }
    }
}

impl Default for FabricMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- InferenceTransport tests ---

    #[test]
    fn deterministic_transport_executes_successfully() {
        let transport = DeterministicTransport {
            healthy: true,
            latency_ms: 42,
            capabilities: vec!["vllm".into()],
        };
        let request = TransportRequest {
            request_id: "req-1".into(),
            model: "llama-3".into(),
            model_version: "v1".into(),
            backend_id: "gpu-0".into(),
            timeout: Duration::from_secs(5),
        };
        let resp = transport.execute(&request).unwrap();
        assert_eq!(resp.request_id, "req-1");
        assert_eq!(resp.backend_id, "gpu-0");
        assert!(resp.success);
        assert_eq!(resp.latency_ms, 42);
        assert!(transport.health());
        assert_eq!(transport.capabilities(), vec!["vllm"]);
    }

    #[test]
    fn unhealthy_transport_rejects_execution() {
        let transport = DeterministicTransport {
            healthy: false,
            latency_ms: 0,
            capabilities: vec![],
        };
        let request = TransportRequest {
            request_id: "req-2".into(),
            model: "m".into(),
            model_version: "v1".into(),
            backend_id: "b".into(),
            timeout: Duration::from_secs(1),
        };
        assert_eq!(
            transport.execute(&request).unwrap_err(),
            FabricError::BackendUnavailable
        );
    }

    // --- KV decision tests ---

    #[test]
    fn incompatible_kv_always_recomputes() {
        let input = KvDecisionInput {
            compatible: false,
            kv_size_mb: 100,
            transfer_latency_ms: 5,
            network_cost: 0.01,
            recomputation_ms: 500,
            expected_reuse: 0.9,
            failure_risk: 0.0,
            confidence: 1.0,
        };
        assert_eq!(input.decide(), (KvAction::Recompute, "INCOMPATIBLE_KV"));
    }

    #[test]
    fn low_confidence_kv_recomputes() {
        let input = KvDecisionInput {
            compatible: true,
            kv_size_mb: 10,
            transfer_latency_ms: 5,
            network_cost: 0.01,
            recomputation_ms: 500,
            expected_reuse: 0.9,
            failure_risk: 0.0,
            confidence: 0.3,
        };
        assert_eq!(input.decide(), (KvAction::Recompute, "LOW_CONFIDENCE"));
    }

    #[test]
    fn high_risk_kv_recomputes() {
        let input = KvDecisionInput {
            compatible: true,
            kv_size_mb: 10,
            transfer_latency_ms: 5,
            network_cost: 0.01,
            recomputation_ms: 500,
            expected_reuse: 0.9,
            failure_risk: 0.5,
            confidence: 0.9,
        };
        assert_eq!(input.decide(), (KvAction::Recompute, "HIGH_FAILURE_RISK"));
    }

    #[test]
    fn beneficial_transfer_selects_transfer() {
        let input = KvDecisionInput {
            compatible: true,
            kv_size_mb: 10,
            transfer_latency_ms: 10,
            network_cost: 0.01,
            recomputation_ms: 500,
            expected_reuse: 0.9,
            failure_risk: 0.01,
            confidence: 0.95,
        };
        assert_eq!(input.decide(), (KvAction::Transfer, "TRANSFER_BENEFICIAL"));
    }

    // --- Admission control tests ---

    #[test]
    fn admission_respects_concurrency_and_queue_limits() {
        let ac = AdmissionController::new(2, 1);
        assert_eq!(ac.evaluate().0, AdmissionDecision::Accept);
        assert_eq!(ac.evaluate().0, AdmissionDecision::Accept);
        assert_eq!(ac.evaluate().0, AdmissionDecision::Queue);
        let (decision, rejection) = ac.evaluate();
        assert_eq!(decision, AdmissionDecision::Shed);
        assert_eq!(rejection, Some(AdmissionRejection::QueueFull));
    }

    #[test]
    fn admission_tracks_active_requests() {
        let ac = AdmissionController::new(2, 0);
        assert_eq!(ac.active_requests(), 0);
        ac.evaluate();
        assert_eq!(ac.active_requests(), 1);
        ac.complete();
        assert_eq!(ac.active_requests(), 0);
    }

    // --- Bounded queue tests ---

    #[test]
    fn bounded_queue_rejects_overflow() {
        let queue = BoundedQueue::new(2);
        assert!(queue.push("a").is_ok());
        assert!(queue.push("b").is_ok());
        assert_eq!(queue.push("c").unwrap_err(), FabricError::QueueFull);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn bounded_queue_fifo_order() {
        let queue = BoundedQueue::new(3);
        queue.push(1).unwrap();
        queue.push(2).unwrap();
        queue.push(3).unwrap();
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(3));
        assert!(queue.is_empty());
    }

    // --- Node lifecycle state machine tests ---

    #[test]
    fn lifecycle_healthy_to_failed_and_recovery() {
        let mut node = NodeLifecycle::new(100);
        assert_eq!(node.state(), NodeLifecycleState::Healthy);

        for _ in 0..5 {
            node.record_failure();
        }
        assert_eq!(node.state(), NodeLifecycleState::Failed);

        node.begin_recovery();
        assert_eq!(node.state(), NodeLifecycleState::Recovering);

        for _ in 0..3 {
            node.record_success();
        }
        assert_eq!(node.state(), NodeLifecycleState::Healthy);
    }

    #[test]
    fn lifecycle_transitions_are_bounded() {
        let mut node = NodeLifecycle::new(3);
        for _ in 0..10 {
            node.record_failure();
            node.begin_recovery();
            node.record_success();
            node.record_success();
            node.record_success();
        }
        assert!(node.transitions().len() <= 3);
    }

    #[test]
    fn lifecycle_drain_transition() {
        let mut node = NodeLifecycle::new(10);
        node.drain();
        assert_eq!(node.state(), NodeLifecycleState::Draining);
        // Draining again is idempotent
        node.drain();
        assert_eq!(node.state(), NodeLifecycleState::Draining);
    }

    // --- Lease / epoch / fencing tests ---

    #[test]
    fn lease_acquisition_and_stale_fencing() {
        let mut store = InMemoryLeaseStore::new();
        let lease1 = store
            .acquire("leader_a", Duration::from_millis(100))
            .unwrap();
        assert!(store.verify(lease1.epoch));

        // Leader B cannot acquire while A is valid
        assert!(store
            .acquire("leader_b", Duration::from_millis(100))
            .is_none());

        // Wait for expiry
        std::thread::sleep(Duration::from_millis(150));
        let lease2 = store
            .acquire("leader_b", Duration::from_millis(100))
            .unwrap();
        assert!(store.verify(lease2.epoch));

        // Leader A's epoch is stale
        assert!(!store.verify(lease1.epoch));
        assert!(lease2.epoch > lease1.epoch);
    }

    #[test]
    fn epoch_monotonicity() {
        let mut store = InMemoryLeaseStore::new();
        let l1 = store.acquire("a", Duration::from_millis(1)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let l2 = store.acquire("b", Duration::from_millis(1)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let l3 = store.acquire("c", Duration::from_millis(100)).unwrap();
        assert!(l1.epoch < l2.epoch);
        assert!(l2.epoch < l3.epoch);
    }

    // --- Backend readiness tests ---

    #[test]
    fn backend_readiness_selection() {
        let ready = RuntimeBackendState {
            backend_id: "b-1".into(),
            readiness: BackendReadiness::Ready,
            model_loaded: true,
            model_version: "v1".into(),
            health_ok: true,
            last_checked: SystemTime::now(),
        };
        assert!(ready.is_selectable());

        let declared = RuntimeBackendState {
            readiness: BackendReadiness::Declared,
            ..ready.clone()
        };
        assert!(!declared.is_selectable());

        let no_model = RuntimeBackendState {
            model_loaded: false,
            ..ready.clone()
        };
        assert!(!no_model.is_selectable());

        let stale = RuntimeBackendState {
            last_checked: SystemTime::now() - Duration::from_secs(600),
            ..ready
        };
        assert!(stale.is_stale(Duration::from_secs(300)));
    }

    // --- Audit chain tests ---

    #[test]
    fn audit_chain_integrity_passes_for_valid_chain() {
        let mut chain = AuditChain::new(100);
        chain.append("evt-1".into(), "operator".into(), 1);
        chain.append("evt-2".into(), "operator".into(), 2);
        chain.append("evt-3".into(), "system".into(), 3);
        assert!(chain.verify_integrity().is_ok());
        assert_eq!(chain.len(), 3);
    }

    #[test]
    fn audit_chain_detects_tampered_record() {
        let mut chain = AuditChain::new(100);
        chain.append("evt-1".into(), "operator".into(), 1);
        chain.append("evt-2".into(), "operator".into(), 2);

        // Tamper with a record
        chain.records[0].current_hash = "fake".into();
        assert_eq!(chain.verify_integrity(), Err("TAMPERED_RECORD"));
    }

    #[test]
    fn audit_chain_detects_broken_link() {
        let mut chain = AuditChain::new(100);
        chain.append("evt-1".into(), "operator".into(), 1);
        chain.append("evt-2".into(), "operator".into(), 2);

        // Break the chain link
        chain.records[1].previous_hash = "wrong_link".into();
        assert!(chain.verify_integrity().is_err());
    }

    #[test]
    fn audit_chain_is_bounded() {
        let mut chain = AuditChain::new(3);
        for i in 0..10 {
            chain.append(format!("evt-{i}"), "op".into(), i as u64);
        }
        assert_eq!(chain.len(), 3);
    }

    // --- Canary tests ---

    #[test]
    fn canary_auto_stop_on_error_rate() {
        let config = CanaryConfig::default();
        assert_eq!(
            config.should_auto_stop(0.1, 100, 0.0),
            Some("ERROR_RATE_EXCEEDED")
        );
        assert_eq!(config.should_auto_stop(0.01, 100, 0.0), None);
    }

    #[test]
    fn canary_auto_stop_on_latency() {
        let config = CanaryConfig::default();
        assert_eq!(
            config.should_auto_stop(0.01, 1000, 0.0),
            Some("P99_LATENCY_EXCEEDED")
        );
    }

    #[test]
    fn canary_promotion_requires_sufficient_samples() {
        let config = CanaryConfig::default();
        assert!(!config.can_promote(50, 0)); // insufficient
        assert!(config.can_promote(100, 0)); // sufficient
        assert!(!config.can_promote(1000, 4)); // already at last stage
    }

    // --- Telemetry validation ---

    #[test]
    fn telemetry_validation_rejects_invalid_gpu_utilization() {
        let mut telemetry = NodeTelemetry {
            timestamp: SystemTime::now(),
            node_id: "node-1".into(),
            revision: 1,
            provider_version: "v1".into(),
            gpu_utilization: 1.5, // invalid
            gpu_memory_used_mb: 4000,
            gpu_memory_total_mb: 8000,
            compute_availability: 0.5,
            queue_depth: 0,
            active_requests: 0,
            model_residency: vec![],
            kv_utilization: 0.0,
        };
        assert!(telemetry.validate().is_err());
        telemetry.gpu_utilization = 0.8;
        assert!(telemetry.validate().is_ok());
    }

    #[test]
    fn stale_telemetry_is_detected() {
        let telemetry = NodeTelemetry {
            timestamp: SystemTime::now() - Duration::from_secs(600),
            node_id: "node-1".into(),
            revision: 1,
            provider_version: "v1".into(),
            gpu_utilization: 0.5,
            gpu_memory_used_mb: 4000,
            gpu_memory_total_mb: 8000,
            compute_availability: 0.5,
            queue_depth: 0,
            active_requests: 0,
            model_residency: vec![],
            kv_utilization: 0.0,
        };
        assert!(telemetry.is_stale(Duration::from_secs(300)));
    }

    // --- Execution compiler ---

    #[test]
    fn execution_compiler_produces_valid_plan() {
        let candidate = CounterfactualEstimate {
            backend_id: "gpu-0".into(),
            compute_node_id: "node-1".into(),
            model: "llama-3".into(),
            strategy: "prefill-decode".into(),
            predicted_ttft: 30,
            predicted_tpot: 50,
            predicted_e2e: 200,
            predicted_cost: 0.01,
            predicted_failure_probability: 0.02,
            predicted_kv_transfer: 5,
            quality_risk: 0.95,
            confidence: 0.9,
            feasible: true,
            rejection_reason: None,
        };
        let kv_decision = (KvAction::Transfer, "TRANSFER_BENEFICIAL");
        let plan = ExecutionCompiler::compile(&candidate, kv_decision, 0.8, 42, 3, 7);
        assert_eq!(plan.schema_version, 2);
        assert_eq!(plan.model, "llama-3");
        assert_eq!(plan.backend, "gpu-0");
        assert_eq!(plan.kv_action, KvAction::Transfer);
        assert_eq!(plan.kv_reason, "TRANSFER_BENEFICIAL");
        assert_eq!(plan.quality_floor, 0.8);
        assert_eq!(plan.world_revision, 42);
        assert_eq!(plan.lease_epoch, 3);
        assert_eq!(plan.fencing_token, 7);
    }

    // --- KV transport ---

    #[test]
    fn deterministic_kv_transport_respects_compatibility() {
        let transport = DeterministicKvTransport {
            allow_transfer: false,
            transfer_latency_per_mb: Duration::from_millis(1),
        };
        assert!(!transport.can_transfer("a", "b"));
        assert_eq!(
            transport.transfer("a", "b").unwrap_err(),
            FabricError::KvTransferFailed
        );

        let ok_transport = DeterministicKvTransport {
            allow_transfer: true,
            transfer_latency_per_mb: Duration::from_millis(2),
        };
        assert!(ok_transport.can_transfer("a", "b"));
        assert!(ok_transport.transfer("a", "b").is_ok());
        assert_eq!(
            ok_transport.estimate_transfer(10),
            Duration::from_millis(20)
        );
    }

    // --- Concurrent admission ---

    #[test]
    fn concurrent_admission_respects_bounds() {
        use std::sync::Arc;
        use std::thread;
        let ac = Arc::new(AdmissionController::new(10, 5));
        let mut handles = vec![];

        for _ in 0..30 {
            let ac = Arc::clone(&ac);
            handles.push(thread::spawn(move || ac.evaluate()));
        }

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let accepted = results
            .iter()
            .filter(|(d, _)| *d == AdmissionDecision::Accept)
            .count();
        let queued = results
            .iter()
            .filter(|(d, _)| *d == AdmissionDecision::Queue)
            .count();
        let shed = results
            .iter()
            .filter(|(d, _)| *d == AdmissionDecision::Shed)
            .count();
        assert!(accepted <= 10);
        assert!(queued <= 5);
        assert!(shed > 0);
        assert_eq!(accepted + queued + shed, 30);
    }

    // --- Version lifecycle ---

    #[test]
    fn version_stages_are_explicit() {
        let artifact = VersionedArtifact {
            name: "predictor".into(),
            version: "v2".into(),
            stage: VersionStage::Canary,
        };
        assert_eq!(artifact.stage, VersionStage::Canary);
    }
}
