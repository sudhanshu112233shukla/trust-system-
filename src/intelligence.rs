use crate::{NodeId, TenantId};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntelligenceError {
    InvalidObservation(&'static str),
    StaleWorldState,
    InsufficientData,
}

impl std::fmt::Display for IntelligenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidObservation(msg) => write!(f, "invalid observation: {msg}"),
            Self::StaleWorldState => write!(f, "stale world state"),
            Self::InsufficientData => write!(f, "insufficient data for prediction"),
        }
    }
}
impl std::error::Error for IntelligenceError {}

#[derive(Debug, Clone, PartialEq)]
pub struct InferenceObservation {
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

impl InferenceObservation {
    pub fn validate(&self) -> Result<(), IntelligenceError> {
        if self.request_id.is_empty() {
            return Err(IntelligenceError::InvalidObservation("empty request_id"));
        }
        if self.gpu_utilization.is_nan() || self.gpu_utilization < 0.0 || self.gpu_utilization > 1.0
        {
            return Err(IntelligenceError::InvalidObservation(
                "invalid gpu_utilization",
            ));
        }
        if self.quality_score.is_nan() || self.quality_score < 0.0 || self.quality_score > 1.0 {
            return Err(IntelligenceError::InvalidObservation(
                "invalid quality_score",
            ));
        }
        if self.actual_cost.is_nan() || self.actual_cost < 0.0 {
            return Err(IntelligenceError::InvalidObservation("invalid actual_cost"));
        }
        Ok(())
    }
}

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
        if let Ok(elapsed) = self.observed_at.elapsed() {
            elapsed > max_age
        } else {
            false
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionConfidence {
    High,
    Medium,
    Low,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct CounterfactualEstimate {
    pub backend_id: String,
    pub compute_node_id: String,
    pub model: String,
    pub strategy: String,
    pub predicted_ttft: u64,
    pub predicted_tpot: u64,
    pub predicted_e2e: u64,
    pub predicted_cost: f64,
    pub predicted_failure_probability: f64,
    pub predicted_kv_transfer: u64,
    pub quality_risk: f64,
    pub confidence: f64,
    pub feasible: bool,
    pub rejection_reason: Option<String>,
}

pub struct DecisionMemory {
    pub max_size: usize,
    pub memory: VecDeque<String>,
}

impl DecisionMemory {
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            memory: VecDeque::new(),
        }
    }

    pub fn record(&mut self, record: String) {
        if self.memory.len() >= self.max_size {
            self.memory.pop_front();
        }
        self.memory.push_back(record);
    }
}

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
    pub fn is_valid(&self) -> bool {
        self.weight_latency >= 0.0
            && self.weight_throughput >= 0.0
            && self.weight_reliability >= 0.0
            && self.weight_quality >= 0.0
            && self.weight_cost >= 0.0
            && self.weight_kv_transfer >= 0.0
    }
}

pub struct DecisionFirewall {
    pub config: UtilityConfig,
}

impl DecisionFirewall {
    pub fn evaluate_candidate(
        &self,
        candidate: &CounterfactualEstimate,
        world_state: &WorldState,
    ) -> Result<(), &'static str> {
        if !self.config.is_valid() {
            return Err("INVALID_CONFIGURATION");
        }
        if world_state.is_stale(Duration::from_secs(300)) {
            return Err("STALE_WORLD_STATE");
        }
        if candidate.predicted_ttft > self.config.max_ttft {
            return Err("SLO_VIOLATION_TTFT");
        }
        if candidate.predicted_tpot > self.config.max_tpot {
            return Err("SLO_VIOLATION_TPOT");
        }
        if candidate.predicted_e2e > self.config.max_e2e {
            return Err("SLO_VIOLATION_E2E");
        }
        if candidate.quality_risk < self.config.min_quality {
            return Err("SLO_VIOLATION_QUALITY");
        }
        if candidate.predicted_cost > self.config.max_cost {
            return Err("SLO_VIOLATION_COST");
        }
        if candidate.predicted_failure_probability > self.config.max_failure_probability {
            return Err("SLO_VIOLATION_RELIABILITY");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_observation() {
        let obs = InferenceObservation {
            request_id: "req-1".into(),
            tenant: "t-1".into(),
            model: "m-1".into(),
            model_version: "v1".into(),
            backend_id: "b-1".into(),
            backend_version: "v1".into(),
            compute_node_id: "c-1".into(),
            accelerator: "a-1".into(),
            region: "r-1".into(),
            prefill_tokens: 10,
            decode_tokens: 20,
            total_tokens: 30,
            queue_delay_ms: 1,
            ttft_ms: 5,
            tpot_ms: 10,
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
            actual_cost: 0.01,
            timestamp: SystemTime::now(),
            world_revision: 1,
        };
        assert!(obs.validate().is_ok());
    }

    #[test]
    fn test_stale_world_state() {
        let ws = WorldState {
            schema_version: 1,
            revision: 1,
            observed_at: SystemTime::now() - Duration::from_secs(10),
            nodes: vec![],
            backends: vec![],
            model_states: HashMap::new(),
            queues: HashMap::new(),
            kv_states: HashMap::new(),
            network_states: HashMap::new(),
            health_states: HashMap::new(),
        };
        assert!(ws.is_stale(Duration::from_secs(5)));
        assert!(!ws.is_stale(Duration::from_secs(15)));
    }

    #[test]
    fn test_decision_memory() {
        let mut dm = DecisionMemory::new(2);
        dm.record("req-1".into());
        dm.record("req-2".into());
        dm.record("req-3".into());
        assert_eq!(dm.memory.len(), 2);
        assert_eq!(dm.memory.front().unwrap(), "req-2");
    }

    #[test]
    fn test_decision_firewall() {
        let config = UtilityConfig {
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
        };
        let firewall = DecisionFirewall { config };

        let mut candidate = CounterfactualEstimate {
            backend_id: "b-1".into(),
            compute_node_id: "c-1".into(),
            model: "m-1".into(),
            strategy: "s-1".into(),
            predicted_ttft: 40,
            predicted_tpot: 90,
            predicted_e2e: 300,
            predicted_cost: 0.02,
            predicted_failure_probability: 0.05,
            predicted_kv_transfer: 10,
            quality_risk: 0.9,
            confidence: 0.95,
            feasible: true,
            rejection_reason: None,
        };

        let ws = WorldState {
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
        };

        // Should pass
        assert_eq!(firewall.evaluate_candidate(&candidate, &ws), Ok(()));

        // Modify to violate TTFT
        candidate.predicted_ttft = 60;
        assert_eq!(
            firewall.evaluate_candidate(&candidate, &ws),
            Err("SLO_VIOLATION_TTFT")
        );
    }
}
