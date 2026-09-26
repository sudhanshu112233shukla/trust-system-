//! Predictive inference intelligence: observations, world state, predictions,
//! counterfactual evaluation, prediction error tracking, decision firewall,
//! and bounded decision memory.
//!
//! All predictions are deterministic given identical inputs. The predictive
//! subsystem never bypasses safety constraints from the decision firewall.

use crate::{NodeId, TenantId};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

// =========================================================================
// Error types
// =========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntelligenceError {
    InvalidObservation(&'static str),
    StaleWorldState,
    InsufficientData,
    InvalidConfiguration(&'static str),
}

impl std::fmt::Display for IntelligenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidObservation(msg) => write!(f, "invalid observation: {msg}"),
            Self::StaleWorldState => write!(f, "stale world state"),
            Self::InsufficientData => write!(f, "insufficient data for prediction"),
            Self::InvalidConfiguration(msg) => write!(f, "invalid configuration: {msg}"),
        }
    }
}

impl std::error::Error for IntelligenceError {}

// =========================================================================
// Inference observation — strongly typed telemetry
// =========================================================================

pub const MAX_IDENTIFIER_LENGTH: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub struct InferenceObservation {
    pub schema_version: u8,
    pub request_id: String,
    pub tenant: TenantId,
    pub model: String,
    pub model_version: String,
    pub backend_id: String,
    pub backend_version: String,
    pub compute_node_id: String,
    pub accelerator: String,
    pub region: String,
    pub prefill_tokens: u64,
    pub decode_tokens: u64,
    pub total_tokens: u64,
    pub queue_delay_ms: u64,
    pub ttft_ms: u64,
    pub tpot_ms: u64,
    pub e2e_latency_ms: u64,
    pub gpu_utilization: f64,
    pub gpu_memory_used_mb: u64,
    pub gpu_memory_total_mb: u64,
    pub kv_hit: bool,
    pub kv_tokens: u64,
    pub kv_memory_mb: u64,
    pub kv_transfer_ms: u64,
    pub network_transfer_ms: u64,
    pub retries: u32,
    pub failure_class: Option<String>,
    pub quality_score: f64,
    pub estimated_cost: f64,
    pub actual_cost: f64,
    pub timestamp: SystemTime,
    pub world_revision: u64,
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_IDENTIFIER_LENGTH
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn valid_f64_unit(v: f64) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

fn valid_f64_nonneg(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}

impl InferenceObservation {
    pub fn validate(&self) -> Result<(), IntelligenceError> {
        if !valid_id(&self.request_id) {
            return Err(IntelligenceError::InvalidObservation("invalid request_id"));
        }
        if !valid_id(&self.tenant) {
            return Err(IntelligenceError::InvalidObservation("invalid tenant"));
        }
        if !valid_id(&self.model) {
            return Err(IntelligenceError::InvalidObservation("invalid model"));
        }
        if !valid_id(&self.backend_id) {
            return Err(IntelligenceError::InvalidObservation("invalid backend_id"));
        }
        if !valid_f64_unit(self.gpu_utilization) {
            return Err(IntelligenceError::InvalidObservation(
                "gpu_utilization must be 0.0..=1.0",
            ));
        }
        if !valid_f64_unit(self.quality_score) {
            return Err(IntelligenceError::InvalidObservation(
                "quality_score must be 0.0..=1.0",
            ));
        }
        if !valid_f64_nonneg(self.estimated_cost) {
            return Err(IntelligenceError::InvalidObservation(
                "estimated_cost must be finite and non-negative",
            ));
        }
        if !valid_f64_nonneg(self.actual_cost) {
            return Err(IntelligenceError::InvalidObservation(
                "actual_cost must be finite and non-negative",
            ));
        }
        Ok(())
    }
}

// =========================================================================
// Versioned world state
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct WorldState {
    pub schema_version: u8,
    pub revision: u64,
    pub observed_at: SystemTime,
    pub nodes: Vec<NodeId>,
    pub backends: Vec<String>,
    pub model_states: HashMap<String, String>,
    pub queues: HashMap<String, u64>,
    pub kv_states: HashMap<String, String>,
    pub network_states: HashMap<String, String>,
    pub health_states: HashMap<String, String>,
}

impl WorldState {
    pub fn is_stale(&self, max_age: Duration) -> bool {
        self.observed_at
            .elapsed()
            .map_or(true, |elapsed| elapsed > max_age)
    }
}

/// Thread-safe world state manager with monotonic revisions.
pub struct WorldStateManager {
    state: Arc<RwLock<WorldState>>,
    max_observation_age: Duration,
    observations_accepted: std::sync::atomic::AtomicU64,
    observations_rejected: std::sync::atomic::AtomicU64,
}

impl WorldStateManager {
    pub fn new(max_observation_age: Duration) -> Self {
        Self {
            state: Arc::new(RwLock::new(WorldState {
                schema_version: 1,
                revision: 0,
                observed_at: SystemTime::now(),
                nodes: vec![],
                backends: vec![],
                model_states: HashMap::new(),
                queues: HashMap::new(),
                kv_states: HashMap::new(),
                network_states: HashMap::new(),
                health_states: HashMap::new(),
            })),
            max_observation_age,
            observations_accepted: std::sync::atomic::AtomicU64::new(0),
            observations_rejected: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Ingest a validated observation and bump the world revision.
    pub fn ingest(&self, obs: &InferenceObservation) -> Result<u64, IntelligenceError> {
        obs.validate()?;
        let mut state = self.state.write().expect("world state lock poisoned");
        state.revision += 1;
        state.observed_at = SystemTime::now();
        if !state.nodes.contains(&obs.compute_node_id) {
            state.nodes.push(obs.compute_node_id.clone());
        }
        if !state.backends.contains(&obs.backend_id) {
            state.backends.push(obs.backend_id.clone());
        }
        state
            .health_states
            .insert(obs.backend_id.clone(), "observed".into());
        self.observations_accepted
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(state.revision)
    }

    /// Returns an immutable snapshot. Fails if the state is stale.
    pub fn snapshot(&self) -> Result<WorldState, IntelligenceError> {
        let state = self.state.read().expect("world state lock poisoned");
        if state.is_stale(self.max_observation_age) {
            return Err(IntelligenceError::StaleWorldState);
        }
        Ok(state.clone())
    }

    /// Returns the current revision without a freshness check.
    pub fn revision(&self) -> u64 {
        self.state
            .read()
            .expect("world state lock poisoned")
            .revision
    }

    pub fn accepted_count(&self) -> u64 {
        self.observations_accepted
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn rejected_count(&self) -> u64 {
        self.observations_rejected
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

// =========================================================================
// Prediction engine — dependency-light online statistics
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct Prediction<T> {
    pub value: T,
    pub lower_bound: T,
    pub upper_bound: T,
    pub confidence: f64,
    pub sample_count: u64,
    pub model_version: String,
    pub generated_at: SystemTime,
    pub source_revision: u64,
}

/// Exponentially weighted moving average tracker with bounded sample history.
#[derive(Debug, Clone)]
pub struct EwmaTracker {
    alpha: f64,
    mean: f64,
    variance: f64,
    sample_count: u64,
    min_samples: u64,
    model_version: String,
}

impl EwmaTracker {
    pub fn new(alpha: f64, min_samples: u64) -> Self {
        Self {
            alpha: alpha.clamp(0.01, 0.99),
            mean: 0.0,
            variance: 0.0,
            sample_count: 0,
            min_samples: min_samples.max(1),
            model_version: "ewma-v1".into(),
        }
    }

    pub fn update(&mut self, value: f64) {
        if !value.is_finite() || value < 0.0 {
            return; // reject invalid samples silently
        }
        self.sample_count += 1;
        if self.sample_count == 1 {
            self.mean = value;
            self.variance = 0.0;
        } else {
            let delta = value - self.mean;
            self.mean += self.alpha * delta;
            self.variance = (1.0 - self.alpha) * (self.variance + self.alpha * delta * delta);
        }
    }

    pub fn predict(&self, source_revision: u64) -> Result<Prediction<f64>, IntelligenceError> {
        if self.sample_count < self.min_samples {
            return Err(IntelligenceError::InsufficientData);
        }
        let stddev = self.variance.sqrt();
        let confidence =
            (self.sample_count as f64 / (self.sample_count as f64 + 10.0)).clamp(0.0, 1.0);
        Ok(Prediction {
            value: self.mean,
            lower_bound: (self.mean - 2.0 * stddev).max(0.0),
            upper_bound: self.mean + 2.0 * stddev,
            confidence,
            sample_count: self.sample_count,
            model_version: self.model_version.clone(),
            generated_at: SystemTime::now(),
            source_revision,
        })
    }

    pub fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub fn mean(&self) -> f64 {
        self.mean
    }
}

/// Prediction engine managing trackers for multiple metrics per backend.
pub struct PredictionEngine {
    trackers: HashMap<String, EwmaTracker>,
    alpha: f64,
    min_samples: u64,
}

impl PredictionEngine {
    pub fn new(alpha: f64, min_samples: u64) -> Self {
        Self {
            trackers: HashMap::new(),
            alpha,
            min_samples,
        }
    }

    pub fn observe(&mut self, key: &str, value: f64) {
        let tracker = self
            .trackers
            .entry(key.to_string())
            .or_insert_with(|| EwmaTracker::new(self.alpha, self.min_samples));
        tracker.update(value);
    }

    pub fn predict(
        &self,
        key: &str,
        source_revision: u64,
    ) -> Result<Prediction<f64>, IntelligenceError> {
        self.trackers
            .get(key)
            .ok_or(IntelligenceError::InsufficientData)?
            .predict(source_revision)
    }

    pub fn tracker_count(&self) -> usize {
        self.trackers.len()
    }
}

// =========================================================================
// Prediction error tracker
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct PredictionError {
    pub metric: String,
    pub predicted: f64,
    pub actual: f64,
    pub absolute_error: f64,
    pub relative_error: f64,
    pub confidence: f64,
    pub context: String,
    pub prediction_model_version: String,
    pub observation_revision: u64,
}

impl PredictionError {
    pub fn compute(
        metric: &str,
        predicted: f64,
        actual: f64,
        confidence: f64,
        model_version: &str,
        observation_revision: u64,
    ) -> Self {
        let absolute_error = (predicted - actual).abs();
        let relative_error = if actual.abs() > f64::EPSILON {
            absolute_error / actual.abs()
        } else {
            0.0
        };
        Self {
            metric: metric.to_string(),
            predicted,
            actual,
            absolute_error,
            relative_error,
            confidence,
            context: String::new(),
            prediction_model_version: model_version.to_string(),
            observation_revision,
        }
    }
}

pub struct PredictionErrorTracker {
    errors: VecDeque<PredictionError>,
    max_errors: usize,
}

impl PredictionErrorTracker {
    pub fn new(max_errors: usize) -> Self {
        Self {
            errors: VecDeque::new(),
            max_errors: max_errors.clamp(1, 100_000),
        }
    }

    pub fn record(&mut self, error: PredictionError) {
        if self.errors.len() >= self.max_errors {
            self.errors.pop_front();
        }
        self.errors.push_back(error);
    }

    pub fn mae(&self) -> f64 {
        if self.errors.is_empty() {
            return 0.0;
        }
        self.errors.iter().map(|e| e.absolute_error).sum::<f64>() / self.errors.len() as f64
    }

    pub fn rmse(&self) -> f64 {
        if self.errors.is_empty() {
            return 0.0;
        }
        let mse = self
            .errors
            .iter()
            .map(|e| e.absolute_error * e.absolute_error)
            .sum::<f64>()
            / self.errors.len() as f64;
        mse.sqrt()
    }

    pub fn count(&self) -> usize {
        self.errors.len()
    }

    pub fn p50_error(&self) -> f64 {
        self.percentile_error(0.50)
    }

    pub fn p95_error(&self) -> f64 {
        self.percentile_error(0.95)
    }

    fn percentile_error(&self, p: f64) -> f64 {
        if self.errors.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f64> = self.errors.iter().map(|e| e.absolute_error).collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let index = ((sorted.len().saturating_sub(1)) as f64 * p).round() as usize;
        sorted[index]
    }
}

// =========================================================================
// Decision confidence
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionConfidence {
    High,
    Medium,
    Low,
    Unknown,
}

impl DecisionConfidence {
    pub fn from_score(confidence: f64, sample_count: u64) -> Self {
        if sample_count < 5 {
            return Self::Unknown;
        }
        if confidence >= 0.8 {
            Self::High
        } else if confidence >= 0.5 {
            Self::Medium
        } else {
            Self::Low
        }
    }

    pub fn is_safe_for_optimization(&self) -> bool {
        matches!(self, Self::High | Self::Medium)
    }
}

// =========================================================================
// Counterfactual engine
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum OperationalValue<T> {
    Observed(T),
    Predicted(T),
    Derived(T),
    Configured(T),
    Unknown,
    Unavailable,
}

impl<T: Copy> OperationalValue<T> {
    pub fn value(&self) -> Option<T> {
        match self {
            Self::Observed(v) | Self::Predicted(v) | Self::Derived(v) | Self::Configured(v) => {
                Some(*v)
            }
            Self::Unknown | Self::Unavailable => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CounterfactualEstimate {
    pub backend_id: String,
    pub compute_node_id: String,
    pub model: String,
    pub strategy: String,
    pub predicted_ttft: OperationalValue<u64>,
    pub predicted_tpot: OperationalValue<u64>,
    pub predicted_e2e: OperationalValue<u64>,
    pub predicted_cost: OperationalValue<f64>,
    pub predicted_failure_probability: OperationalValue<f64>,
    pub predicted_kv_transfer: OperationalValue<u64>,
    pub quality_risk: OperationalValue<f64>,
    pub confidence: OperationalValue<f64>,
    pub feasible: bool,
    pub rejection_reason: Option<String>,
}

/// Indexed counterfactual preserving candidate identity through scoring.
#[derive(Debug)]
pub struct IndexedEstimate {
    pub index: usize,
    pub estimate: CounterfactualEstimate,
}

pub struct CounterfactualEngine;

impl CounterfactualEngine {
    /// Evaluate multiple candidates. Identity is preserved via index, not value equality.
    pub fn evaluate(candidates: Vec<CounterfactualEstimate>) -> Vec<IndexedEstimate> {
        let mut indexed: Vec<IndexedEstimate> = candidates
            .into_iter()
            .enumerate()
            .filter(|(_, c)| c.feasible)
            .map(|(index, estimate)| IndexedEstimate { index, estimate })
            .collect();
        // Deterministic sort: lower score = better. Ties broken by index.
        indexed.sort_by(|a, b| {
            score(&a.estimate)
                .total_cmp(&score(&b.estimate))
                .then_with(|| a.estimate.backend_id.cmp(&b.estimate.backend_id))
                .then_with(|| a.estimate.compute_node_id.cmp(&b.estimate.compute_node_id))
                .then_with(|| a.index.cmp(&b.index))
        });
        indexed
    }

    /// Identify when top candidates are statistically indistinguishable.
    pub fn are_indistinguishable(a: &CounterfactualEstimate, b: &CounterfactualEstimate) -> bool {
        let diff = (score(a) - score(b)).abs();
        diff < 0.01 // within 1% normalized score
    }
}

fn score(c: &CounterfactualEstimate) -> f64 {
    let latency = c.predicted_e2e.value().unwrap_or(u64::MAX) as f64 / 1000.0;
    let cost = c.predicted_cost.value().unwrap_or(f64::INFINITY);
    let risk = c.predicted_failure_probability.value().unwrap_or(1.0);
    let quality = c.quality_risk.value().unwrap_or(1.0);
    let quality_penalty = 1.0 - quality.clamp(0.0, 1.0);

    latency + cost + risk + quality_penalty
}

// =========================================================================
// Decision firewall
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct UtilityConfig {
    pub weight_latency: f64,
    pub weight_throughput: f64,
    pub weight_reliability: f64,
    pub weight_quality: f64,
    pub weight_cost: f64,
    pub weight_kv_transfer: f64,
    pub max_ttft: u64,
    pub max_tpot: u64,
    pub max_e2e: u64,
    pub min_quality: f64,
    pub max_cost: f64,
    pub max_failure_probability: f64,
}

impl UtilityConfig {
    pub fn validate(&self) -> Result<(), IntelligenceError> {
        let weights = [
            self.weight_latency,
            self.weight_throughput,
            self.weight_reliability,
            self.weight_quality,
            self.weight_cost,
            self.weight_kv_transfer,
        ];
        if weights.iter().any(|w| !w.is_finite() || *w < 0.0) {
            return Err(IntelligenceError::InvalidConfiguration(
                "weights must be finite and non-negative",
            ));
        }
        if !self.min_quality.is_finite() || !(0.0..=1.0).contains(&self.min_quality) {
            return Err(IntelligenceError::InvalidConfiguration(
                "min_quality must be 0.0..=1.0",
            ));
        }
        if !self.max_cost.is_finite() || self.max_cost < 0.0 {
            return Err(IntelligenceError::InvalidConfiguration(
                "max_cost must be finite and non-negative",
            ));
        }
        if !self.max_failure_probability.is_finite()
            || !(0.0..=1.0).contains(&self.max_failure_probability)
        {
            return Err(IntelligenceError::InvalidConfiguration(
                "max_failure_probability must be 0.0..=1.0",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallDecision {
    Approved,
    Rejected(&'static str),
}

pub struct DecisionFirewall {
    pub config: UtilityConfig,
    pub max_world_state_age: Duration,
}

impl DecisionFirewall {
    pub fn evaluate_candidate(
        &self,
        candidate: &CounterfactualEstimate,
        world_state: &WorldState,
        confidence: DecisionConfidence,
    ) -> FirewallDecision {
        if self.config.validate().is_err() {
            return FirewallDecision::Rejected("INVALID_CONFIGURATION");
        }
        if world_state.is_stale(self.max_world_state_age) {
            return FirewallDecision::Rejected("STALE_WORLD_STATE");
        }
        if !candidate.feasible {
            return FirewallDecision::Rejected("INFEASIBLE_CANDIDATE");
        }
        if candidate
            .predicted_ttft
            .value()
            .map_or(false, |v| v > self.config.max_ttft)
        {
            return FirewallDecision::Rejected("SLO_VIOLATION_TTFT");
        }
        if candidate
            .predicted_tpot
            .value()
            .map_or(false, |v| v > self.config.max_tpot)
        {
            return FirewallDecision::Rejected("SLO_VIOLATION_TPOT");
        }
        if candidate
            .predicted_e2e
            .value()
            .map_or(false, |v| v > self.config.max_e2e)
        {
            return FirewallDecision::Rejected("SLO_VIOLATION_E2E");
        }
        if candidate
            .quality_risk
            .value()
            .map_or(true, |v| v < self.config.min_quality)
        {
            return FirewallDecision::Rejected("SLO_VIOLATION_QUALITY");
        }
        if candidate
            .predicted_cost
            .value()
            .map_or(false, |v| v > self.config.max_cost)
        {
            return FirewallDecision::Rejected("SLO_VIOLATION_COST");
        }
        if candidate
            .predicted_failure_probability
            .value()
            .map_or(false, |v| v > self.config.max_failure_probability)
        {
            return FirewallDecision::Rejected("SLO_VIOLATION_RELIABILITY");
        }
        if !confidence.is_safe_for_optimization() {
            return FirewallDecision::Rejected("LOW_CONFIDENCE");
        }
        FirewallDecision::Approved
    }
}

// =========================================================================
// Bounded decision memory
// =========================================================================

#[derive(Debug, Clone)]
pub struct DecisionRecord {
    pub request_id: String,
    pub world_revision: u64,
    pub selected_backend: String,
    pub confidence: DecisionConfidence,
    pub utility_score: f64,
    pub decision_reason: String,
    pub timestamp: SystemTime,
}

pub struct DecisionMemory {
    records: VecDeque<DecisionRecord>,
    max_records: usize,
}

impl DecisionMemory {
    pub fn new(max_records: usize) -> Self {
        Self {
            records: VecDeque::new(),
            max_records: max_records.clamp(1, 100_000),
        }
    }

    pub fn record(&mut self, record: DecisionRecord) {
        if self.records.len() >= self.max_records {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    pub fn lookup(&self, request_id: &str) -> Option<&DecisionRecord> {
        self.records.iter().rfind(|r| r.request_id == request_id)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_observation() -> InferenceObservation {
        InferenceObservation {
            schema_version: 1,
            request_id: "req-1".into(),
            tenant: "t-1".into(),
            model: "m-1".into(),
            model_version: "v1".into(),
            backend_id: "b-1".into(),
            backend_version: "v1".into(),
            compute_node_id: "c-1".into(),
            accelerator: "a100".into(),
            region: "us-east".into(),
            prefill_tokens: 100,
            decode_tokens: 200,
            total_tokens: 300,
            queue_delay_ms: 5,
            ttft_ms: 30,
            tpot_ms: 50,
            e2e_latency_ms: 200,
            gpu_utilization: 0.8,
            gpu_memory_used_mb: 4000,
            gpu_memory_total_mb: 8000,
            kv_hit: true,
            kv_tokens: 100,
            kv_memory_mb: 100,
            kv_transfer_ms: 5,
            network_transfer_ms: 2,
            retries: 0,
            failure_class: None,
            quality_score: 0.9,
            estimated_cost: 0.01,
            actual_cost: 0.012,
            timestamp: SystemTime::now(),
            world_revision: 1,
        }
    }

    fn sample_candidate() -> CounterfactualEstimate {
        CounterfactualEstimate {
            backend_id: "b-1".into(),
            compute_node_id: "c-1".into(),
            model: "m-1".into(),
            strategy: "prefill-decode".into(),
            predicted_ttft: OperationalValue::Predicted(30),
            predicted_tpot: OperationalValue::Predicted(50),
            predicted_e2e: OperationalValue::Predicted(200),
            predicted_cost: OperationalValue::Derived(0.02),
            predicted_failure_probability: OperationalValue::Predicted(0.05),
            predicted_kv_transfer: OperationalValue::Predicted(5),
            quality_risk: OperationalValue::Configured(0.9),
            confidence: OperationalValue::Derived(0.95),
            feasible: true,
            rejection_reason: None,
        }
    }

    fn sample_utility_config() -> UtilityConfig {
        UtilityConfig {
            weight_latency: 1.0,
            weight_throughput: 1.0,
            weight_reliability: 1.0,
            weight_quality: 1.0,
            weight_cost: 1.0,
            weight_kv_transfer: 1.0,
            max_ttft: 50,
            max_tpot: 100,
            max_e2e: 500,
            min_quality: 0.8,
            max_cost: 0.05,
            max_failure_probability: 0.1,
        }
    }

    fn fresh_world_state() -> WorldState {
        WorldState {
            schema_version: 1,
            revision: 1,
            observed_at: SystemTime::now(),
            nodes: vec![],
            backends: vec![],
            model_states: HashMap::new(),
            queues: HashMap::new(),
            kv_states: HashMap::new(),
            network_states: HashMap::new(),
            health_states: HashMap::new(),
        }
    }

    // --- Observation validation ---

    #[test]
    fn valid_observation_passes() {
        assert!(sample_observation().validate().is_ok());
    }

    #[test]
    fn observation_rejects_empty_request_id() {
        let mut obs = sample_observation();
        obs.request_id = "".into();
        assert!(matches!(
            obs.validate(),
            Err(IntelligenceError::InvalidObservation(_))
        ));
    }

    #[test]
    fn observation_rejects_nan_gpu_utilization() {
        let mut obs = sample_observation();
        obs.gpu_utilization = f64::NAN;
        assert!(obs.validate().is_err());
    }

    #[test]
    fn observation_rejects_negative_cost() {
        let mut obs = sample_observation();
        obs.actual_cost = -1.0;
        assert!(obs.validate().is_err());
    }

    #[test]
    fn observation_rejects_infinite_quality() {
        let mut obs = sample_observation();
        obs.quality_score = f64::INFINITY;
        assert!(obs.validate().is_err());
    }

    #[test]
    fn observation_rejects_oversized_identifier() {
        let mut obs = sample_observation();
        obs.request_id = "x".repeat(200);
        assert!(obs.validate().is_err());
    }

    #[test]
    fn observation_rejects_log_injection() {
        let mut obs = sample_observation();
        obs.tenant = "tenant\ninjection".into();
        assert!(obs.validate().is_err());
    }

    // --- WorldState / WorldStateManager ---

    #[test]
    fn world_state_stale_detection() {
        let ws = WorldState {
            observed_at: SystemTime::now() - Duration::from_secs(10),
            ..fresh_world_state()
        };
        assert!(ws.is_stale(Duration::from_secs(5)));
        assert!(!ws.is_stale(Duration::from_secs(15)));
    }

    #[test]
    fn world_state_revision_monotonicity() {
        let mgr = WorldStateManager::new(Duration::from_secs(600));
        let obs = sample_observation();
        let r1 = mgr.ingest(&obs).unwrap();
        let r2 = mgr.ingest(&obs).unwrap();
        let r3 = mgr.ingest(&obs).unwrap();
        assert!(r1 < r2);
        assert!(r2 < r3);
    }

    #[test]
    fn world_state_snapshot_is_immutable() {
        let mgr = WorldStateManager::new(Duration::from_secs(600));
        mgr.ingest(&sample_observation()).unwrap();
        let snap1 = mgr.snapshot().unwrap();
        mgr.ingest(&sample_observation()).unwrap();
        let snap2 = mgr.snapshot().unwrap();
        assert!(snap1.revision < snap2.revision);
    }

    #[test]
    fn world_state_rejects_invalid_observation() {
        let mgr = WorldStateManager::new(Duration::from_secs(600));
        let mut obs = sample_observation();
        obs.request_id = "".into();
        assert!(mgr.ingest(&obs).is_err());
    }

    #[test]
    fn world_state_concurrent_observations() {
        use std::sync::Arc;
        use std::thread;
        let mgr = Arc::new(WorldStateManager::new(Duration::from_secs(600)));
        let mut handles = vec![];
        for _ in 0..10 {
            let mgr = Arc::clone(&mgr);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _ = mgr.ingest(&sample_observation());
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(mgr.accepted_count(), 1000);
        assert_eq!(mgr.revision(), 1000);
    }

    // --- EWMA prediction engine ---

    #[test]
    fn ewma_insufficient_data_returns_error() {
        let tracker = EwmaTracker::new(0.1, 5);
        assert_eq!(tracker.predict(1), Err(IntelligenceError::InsufficientData));
    }

    #[test]
    fn ewma_prediction_after_sufficient_samples() {
        let mut tracker = EwmaTracker::new(0.3, 3);
        tracker.update(100.0);
        tracker.update(110.0);
        tracker.update(105.0);
        let pred = tracker.predict(1).unwrap();
        assert!(pred.value > 0.0);
        assert!(pred.lower_bound <= pred.value);
        assert!(pred.upper_bound >= pred.value);
        assert!(pred.confidence > 0.0);
        assert_eq!(pred.sample_count, 3);
    }

    #[test]
    fn ewma_rejects_nan_and_negative_samples() {
        let mut tracker = EwmaTracker::new(0.3, 1);
        tracker.update(f64::NAN);
        tracker.update(-1.0);
        assert_eq!(tracker.sample_count(), 0);
        tracker.update(42.0);
        assert_eq!(tracker.sample_count(), 1);
    }

    #[test]
    fn prediction_engine_manages_multiple_metrics() {
        let mut engine = PredictionEngine::new(0.3, 2);
        engine.observe("ttft:b-1", 30.0);
        engine.observe("ttft:b-1", 35.0);
        engine.observe("tpot:b-1", 50.0);
        engine.observe("tpot:b-1", 55.0);
        assert!(engine.predict("ttft:b-1", 1).is_ok());
        assert!(engine.predict("tpot:b-1", 1).is_ok());
        assert!(engine.predict("unknown", 1).is_err());
        assert_eq!(engine.tracker_count(), 2);
    }

    #[test]
    fn prediction_deterministic_for_identical_input() {
        let mut t1 = EwmaTracker::new(0.3, 2);
        let mut t2 = EwmaTracker::new(0.3, 2);
        for v in [100.0, 110.0, 105.0] {
            t1.update(v);
            t2.update(v);
        }
        // Mean and variance should be identical
        assert!((t1.mean() - t2.mean()).abs() < f64::EPSILON);
    }

    // --- Prediction error tracker ---

    #[test]
    fn prediction_error_exact_calculations() {
        let err = PredictionError::compute("ttft", 100.0, 110.0, 0.9, "ewma-v1", 5);
        assert!((err.absolute_error - 10.0).abs() < f64::EPSILON);
        assert!((err.relative_error - 10.0 / 110.0).abs() < 1e-10);
    }

    #[test]
    fn prediction_error_zero_actual() {
        let err = PredictionError::compute("ttft", 5.0, 0.0, 0.9, "v1", 1);
        assert!((err.relative_error - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn prediction_error_tracker_bounded_retention() {
        let mut tracker = PredictionErrorTracker::new(3);
        for i in 0..10 {
            tracker.record(PredictionError::compute(
                "ttft",
                i as f64,
                (i + 1) as f64,
                0.9,
                "v1",
                i as u64,
            ));
        }
        assert_eq!(tracker.count(), 3);
    }

    #[test]
    fn prediction_error_mae_and_rmse() {
        let mut tracker = PredictionErrorTracker::new(100);
        tracker.record(PredictionError::compute("m", 10.0, 12.0, 0.9, "v1", 1));
        tracker.record(PredictionError::compute("m", 20.0, 18.0, 0.9, "v1", 2));
        assert!((tracker.mae() - 2.0).abs() < f64::EPSILON);
        assert!((tracker.rmse() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn prediction_error_percentiles() {
        let mut tracker = PredictionErrorTracker::new(100);
        for i in 1..=100 {
            tracker.record(PredictionError::compute(
                "m", 0.0, i as f64, 0.9, "v1", i as u64,
            ));
        }
        assert!(tracker.p50_error() > 0.0);
        assert!(tracker.p95_error() > tracker.p50_error());
    }

    // --- Decision confidence ---

    #[test]
    fn confidence_from_score() {
        assert_eq!(
            DecisionConfidence::from_score(0.9, 10),
            DecisionConfidence::High
        );
        assert_eq!(
            DecisionConfidence::from_score(0.6, 10),
            DecisionConfidence::Medium
        );
        assert_eq!(
            DecisionConfidence::from_score(0.3, 10),
            DecisionConfidence::Low
        );
        assert_eq!(
            DecisionConfidence::from_score(0.9, 2),
            DecisionConfidence::Unknown
        );
    }

    #[test]
    fn low_confidence_blocks_optimization() {
        assert!(!DecisionConfidence::Low.is_safe_for_optimization());
        assert!(!DecisionConfidence::Unknown.is_safe_for_optimization());
        assert!(DecisionConfidence::High.is_safe_for_optimization());
    }

    // --- Counterfactual engine ---

    #[test]
    fn counterfactual_preserves_candidate_identity() {
        let c1 = CounterfactualEstimate {
            predicted_e2e: OperationalValue::Predicted(100),
            ..sample_candidate()
        };
        let c2 = CounterfactualEstimate {
            predicted_e2e: OperationalValue::Predicted(200),
            backend_id: "b-2".into(),
            ..sample_candidate()
        };
        let results = CounterfactualEngine::evaluate(vec![c1, c2]);
        assert_eq!(results[0].index, 0); // c1 is better
        assert_eq!(results[1].index, 1);
    }

    #[test]
    fn counterfactual_filters_infeasible() {
        let mut c = sample_candidate();
        c.feasible = false;
        let results = CounterfactualEngine::evaluate(vec![c]);
        assert!(results.is_empty());
    }

    #[test]
    fn counterfactual_deterministic_ordering() {
        let c1 = CounterfactualEstimate {
            predicted_e2e: OperationalValue::Predicted(200),
            backend_id: "b-1".into(),
            ..sample_candidate()
        };
        let c2 = CounterfactualEstimate {
            predicted_e2e: OperationalValue::Predicted(200),
            backend_id: "b-2".into(),
            ..sample_candidate()
        };
        let r1 = CounterfactualEngine::evaluate(vec![c1.clone(), c2.clone()]);
        let r2 = CounterfactualEngine::evaluate(vec![c2, c1]);
        // Same candidates, different input order, but identity preserved
        assert_eq!(r1[0].index, 0);
        assert_eq!(r2[0].index, 1); // c1 was index 1 in second call
    }

    #[test]
    fn counterfactual_indistinguishable_detection() {
        let c1 = sample_candidate();
        let c2 = CounterfactualEstimate {
            ..sample_candidate()
        };
        assert!(CounterfactualEngine::are_indistinguishable(&c1, &c2));
    }

    // --- Decision firewall ---

    #[test]
    fn firewall_approves_valid_candidate() {
        let fw = DecisionFirewall {
            config: sample_utility_config(),
            max_world_state_age: Duration::from_secs(300),
        };
        assert_eq!(
            fw.evaluate_candidate(
                &sample_candidate(),
                &fresh_world_state(),
                DecisionConfidence::High
            ),
            FirewallDecision::Approved
        );
    }

    #[test]
    fn firewall_rejects_stale_world_state() {
        let fw = DecisionFirewall {
            config: sample_utility_config(),
            max_world_state_age: Duration::from_secs(5),
        };
        let ws = WorldState {
            observed_at: SystemTime::now() - Duration::from_secs(10),
            ..fresh_world_state()
        };
        assert_eq!(
            fw.evaluate_candidate(&sample_candidate(), &ws, DecisionConfidence::High),
            FirewallDecision::Rejected("STALE_WORLD_STATE")
        );
    }

    #[test]
    fn firewall_rejects_slo_ttft_violation() {
        let fw = DecisionFirewall {
            config: sample_utility_config(),
            max_world_state_age: Duration::from_secs(300),
        };
        let mut c = sample_candidate();
        c.predicted_ttft = OperationalValue::Predicted(60);
        assert_eq!(
            fw.evaluate_candidate(&c, &fresh_world_state(), DecisionConfidence::High),
            FirewallDecision::Rejected("SLO_VIOLATION_TTFT")
        );
    }

    #[test]
    fn firewall_rejects_slo_cost_violation() {
        let fw = DecisionFirewall {
            config: sample_utility_config(),
            max_world_state_age: Duration::from_secs(300),
        };
        let mut c = sample_candidate();
        c.predicted_cost = OperationalValue::Predicted(1.0);
        assert_eq!(
            fw.evaluate_candidate(&c, &fresh_world_state(), DecisionConfidence::High),
            FirewallDecision::Rejected("SLO_VIOLATION_COST")
        );
    }

    #[test]
    fn firewall_rejects_low_confidence() {
        let fw = DecisionFirewall {
            config: sample_utility_config(),
            max_world_state_age: Duration::from_secs(300),
        };
        assert_eq!(
            fw.evaluate_candidate(
                &sample_candidate(),
                &fresh_world_state(),
                DecisionConfidence::Low
            ),
            FirewallDecision::Rejected("LOW_CONFIDENCE")
        );
    }

    #[test]
    fn firewall_rejects_infeasible_candidate() {
        let fw = DecisionFirewall {
            config: sample_utility_config(),
            max_world_state_age: Duration::from_secs(300),
        };
        let mut c = sample_candidate();
        c.feasible = false;
        assert_eq!(
            fw.evaluate_candidate(&c, &fresh_world_state(), DecisionConfidence::High),
            FirewallDecision::Rejected("INFEASIBLE_CANDIDATE")
        );
    }

    #[test]
    fn firewall_rejects_invalid_config() {
        let mut config = sample_utility_config();
        config.weight_latency = -1.0;
        let fw = DecisionFirewall {
            config,
            max_world_state_age: Duration::from_secs(300),
        };
        assert_eq!(
            fw.evaluate_candidate(
                &sample_candidate(),
                &fresh_world_state(),
                DecisionConfidence::High
            ),
            FirewallDecision::Rejected("INVALID_CONFIGURATION")
        );
    }

    // --- Decision memory ---

    #[test]
    fn decision_memory_bounded() {
        let mut mem = DecisionMemory::new(2);
        for i in 0..5 {
            mem.record(DecisionRecord {
                request_id: format!("req-{i}"),
                world_revision: i,
                selected_backend: "b-1".into(),
                confidence: DecisionConfidence::High,
                utility_score: 0.5,
                decision_reason: "ok".into(),
                timestamp: SystemTime::now(),
            });
        }
        assert_eq!(mem.len(), 2);
    }

    #[test]
    fn decision_memory_lookup() {
        let mut mem = DecisionMemory::new(10);
        mem.record(DecisionRecord {
            request_id: "req-42".into(),
            world_revision: 1,
            selected_backend: "b-1".into(),
            confidence: DecisionConfidence::High,
            utility_score: 0.5,
            decision_reason: "ok".into(),
            timestamp: SystemTime::now(),
        });
        assert!(mem.lookup("req-42").is_some());
        assert!(mem.lookup("req-99").is_none());
    }

    #[test]
    fn decision_memory_no_secrets_stored() {
        let record = DecisionRecord {
            request_id: "req-1".into(),
            world_revision: 1,
            selected_backend: "b-1".into(),
            confidence: DecisionConfidence::High,
            utility_score: 0.5,
            decision_reason: "deterministic_selection".into(),
            timestamp: SystemTime::now(),
        };
        // Verify the struct has no fields for prompts, API keys, or credentials
        let debug = format!("{record:?}");
        assert!(!debug.contains("prompt"));
        assert!(!debug.contains("api_key"));
        assert!(!debug.contains("credential"));
    }

    // --- Integration: observation -> world state -> prediction -> error ---

    #[test]
    fn observation_to_prediction_to_error_pipeline() {
        let mgr = WorldStateManager::new(Duration::from_secs(600));
        let obs = sample_observation();
        let revision = mgr.ingest(&obs).unwrap();

        let mut engine = PredictionEngine::new(0.3, 1);
        engine.observe("ttft:b-1", obs.ttft_ms as f64);
        let pred = engine.predict("ttft:b-1", revision).unwrap();

        let error = PredictionError::compute(
            "ttft",
            pred.value,
            obs.ttft_ms as f64,
            pred.confidence,
            &pred.model_version,
            revision,
        );

        let mut tracker = PredictionErrorTracker::new(100);
        tracker.record(error);
        assert_eq!(tracker.count(), 1);
    }
}
