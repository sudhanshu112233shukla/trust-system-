//! Bounded, serializable explanations for deterministic system-planning decisions.
//!
//! Decision traces are diagnostic records, not durable audit events. They contain
//! no API keys, headers, request bodies, or tool payloads.

use crate::capacity::CapacityRejection;
use crate::inference::DecisionError;
use crate::kv::{KvStrategy, ModelReference};
use crate::system_planning::{PlacementRejection, SystemDecision};
use serde::{Deserialize, Serialize};

pub const DECISION_TRACE_SCHEMA_VERSION: u8 = 1;
pub const MAX_TRACE_REJECTIONS: usize = 64;

/// A stable, bounded diagnostic representation of one selected placement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceSelection {
    pub backend_id: String,
    pub compute_node_id: String,
    pub model: ModelReference,
    pub strategy: KvStrategy,
    pub score: f64,
    pub fallback: Vec<KvStrategy>,
}

/// A redacted, stable reason a candidate could not be selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceRejection {
    pub backend_id: String,
    pub compute_node_id: String,
    pub reason: String,
}

/// A schema-versioned planner explanation suitable for a response or telemetry sink.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionTrace {
    pub schema_version: u8,
    pub request_id: String,
    pub capacity_revision: u64,
    pub backend_revision: u64,
    pub selected: Option<TraceSelection>,
    pub rejected: Vec<TraceRejection>,
    pub rejected_truncated: bool,
    pub unavailable_nodes: Vec<TraceRejection>,
    pub config_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceError {
    InvalidRequestId,
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for TraceError {}

impl DecisionTrace {
    /// Converts a planner decision into a bounded trace with deterministic order.
    pub fn from_system_decision(
        request_id: &str,
        decision: &SystemDecision,
        capacity_revision: u64,
        backend_revision: u64,
        max_rejections: usize,
    ) -> Result<Self, TraceError> {
        if !valid_identifier(request_id) {
            return Err(TraceError::InvalidRequestId);
        }
        let limit = max_rejections.clamp(1, MAX_TRACE_REJECTIONS);
        let selected = decision.selected.as_ref().map(|value| TraceSelection {
            backend_id: value.backend_id.clone(),
            compute_node_id: value.compute_node_id.clone(),
            model: value.scored.candidate.model.clone(),
            strategy: value.scored.candidate.strategy,
            score: value.scored.score,
            fallback: value.fallback.clone(),
        });
        let rejected = decision
            .rejected
            .iter()
            .take(limit)
            .map(|(candidate, reason)| TraceRejection {
                backend_id: candidate.backend_id.clone(),
                compute_node_id: candidate.compute_node_id.clone(),
                reason: rejection_reason(*reason).into(),
            })
            .collect();
        let unavailable_nodes = decision
            .unavailable_nodes
            .iter()
            .map(|(node, reason)| TraceRejection {
                backend_id: String::new(),
                compute_node_id: node.clone(),
                reason: capacity_reason(*reason).into(),
            })
            .collect();
        Ok(Self {
            schema_version: DECISION_TRACE_SCHEMA_VERSION,
            request_id: request_id.into(),
            capacity_revision,
            backend_revision,
            selected,
            rejected,
            rejected_truncated: decision.rejected.len() > limit,
            unavailable_nodes,
            config_error: decision.config_error.map(config_error).map(str::to_owned),
        })
    }
}

fn rejection_reason(value: PlacementRejection) -> &'static str {
    match value {
        PlacementRejection::InvalidCandidate => "invalid_candidate",
        PlacementRejection::ModelMismatch => "model_mismatch",
        PlacementRejection::UnknownComputeNode => "unknown_compute_node",
        PlacementRejection::Capacity(reason) => capacity_reason(reason),
        PlacementRejection::UnknownBackend => "unknown_backend",
        PlacementRejection::UnsupportedBackendCapability => "unsupported_backend_capability",
        PlacementRejection::Inference(reason) => match reason {
            crate::inference::RejectionReason::InvalidEstimate => "invalid_estimate",
            crate::inference::RejectionReason::LatencyConstraint => "latency_constraint",
            crate::inference::RejectionReason::CostConstraint => "cost_constraint",
            crate::inference::RejectionReason::QualityConstraint => "quality_constraint",
            crate::inference::RejectionReason::InvalidPlanningConfig => "invalid_planning_config",
        },
    }
}

fn capacity_reason(value: CapacityRejection) -> &'static str {
    match value {
        CapacityRejection::Unhealthy => "node_unhealthy",
        CapacityRejection::RegionMismatch => "region_mismatch",
        CapacityRejection::ModelNotResident => "model_not_resident",
        CapacityRejection::InsufficientMemory => "insufficient_memory",
        CapacityRejection::QueueLimit => "queue_limit",
    }
}

fn config_error(value: DecisionError) -> &'static str {
    match value {
        DecisionError::InvalidConstraints => "invalid_constraints",
        DecisionError::InvalidObjective => "invalid_objective",
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::{InferenceCandidate, RejectionReason};
    use crate::system_planning::PlacementCandidate;

    fn candidate(id: &str) -> PlacementCandidate {
        PlacementCandidate {
            backend_id: id.into(),
            compute_node_id: "node".into(),
            estimate: InferenceCandidate {
                model: ModelReference::new("model", "1"),
                strategy: KvStrategy::None,
                latency_ms: 1.0,
                cost: 0.1,
                failure_rate: 0.0,
                quality: 1.0,
            },
        }
    }

    #[test]
    fn trace_is_bounded_schema_versioned_and_json_serializable() {
        let decision = SystemDecision {
            selected: None,
            rejected: (0..70)
                .map(|index| {
                    (
                        candidate(&format!("backend-{index}")),
                        PlacementRejection::Inference(RejectionReason::CostConstraint),
                    )
                })
                .collect(),
            unavailable_nodes: vec![("node-down".into(), CapacityRejection::Unhealthy)],
            config_error: None,
        };
        let trace = DecisionTrace::from_system_decision("req-1", &decision, 2, 3, 999).unwrap();
        assert_eq!(trace.schema_version, DECISION_TRACE_SCHEMA_VERSION);
        assert_eq!(trace.rejected.len(), MAX_TRACE_REJECTIONS);
        assert!(trace.rejected_truncated);
        assert_eq!(trace.unavailable_nodes[0].reason, "node_unhealthy");
        let encoded = serde_json::to_string(&trace).unwrap();
        assert_eq!(
            serde_json::from_str::<DecisionTrace>(&encoded).unwrap(),
            trace
        );
    }

    #[test]
    fn trace_rejects_log_injection_request_identifiers() {
        let decision = SystemDecision {
            selected: None,
            rejected: vec![],
            unavailable_nodes: vec![],
            config_error: Some(DecisionError::InvalidConstraints),
        };
        assert_eq!(
            DecisionTrace::from_system_decision("bad\nrequest", &decision, 0, 0, 1),
            Err(TraceError::InvalidRequestId)
        );
    }
}
