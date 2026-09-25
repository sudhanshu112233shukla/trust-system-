//! Deterministic composition of capacity, backend capability, and inference scoring.
//!
//! This module is planning-only. It never executes a backend; it proves that a
//! selected placement has an eligible node and declared backend capability.

use crate::backend_registry::{BackendRegistry, BackendRegistryError};
use crate::capacity::{CapacityRejection, CapacityRequirement, CapacitySnapshot};
use crate::inference::{
    DecisionError, InferenceCandidate, InferenceConstraints, ObjectiveWeights, RejectionReason,
    ScoredCandidate, try_decide,
};
use std::collections::BTreeSet;

/// A backend and compute-node binding for one measured inference estimate.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementCandidate {
    pub backend_id: String,
    pub compute_node_id: String,
    pub estimate: InferenceCandidate,
}

/// A reason a placement could not be considered for deterministic selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlacementRejection {
    InvalidCandidate,
    ModelMismatch,
    UnknownComputeNode,
    Capacity(CapacityRejection),
    UnknownBackend,
    UnsupportedBackendCapability,
    Inference(RejectionReason),
}

/// The selected placement preserves the inference score and fallback order.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementSelection {
    pub backend_id: String,
    pub compute_node_id: String,
    pub scored: ScoredCandidate,
    pub fallback: Vec<crate::kv::KvStrategy>,
}

/// Bounded, reproducible result of all feasibility gates and deterministic scoring.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemDecision {
    pub selected: Option<PlacementSelection>,
    pub rejected: Vec<(PlacementCandidate, PlacementRejection)>,
    pub unavailable_nodes: Vec<(String, CapacityRejection)>,
    pub config_error: Option<DecisionError>,
}

/// Combines immutable snapshots and declared backend capabilities for one decision.
pub struct SystemPlanner<'a> {
    backends: &'a BackendRegistry,
    capacity: &'a CapacitySnapshot,
}

impl<'a> SystemPlanner<'a> {
    pub fn new(backends: &'a BackendRegistry, capacity: &'a CapacitySnapshot) -> Self {
        Self { backends, capacity }
    }

    /// Filters capacity and declared backend capabilities before normalized scoring.
    ///
    /// Invalid scoring configuration fails closed: no candidate is selected and
    /// all otherwise feasible candidates are explicitly marked as rejected.
    pub fn decide(
        &self,
        candidates: &[PlacementCandidate],
        requirement: &CapacityRequirement,
        constraints: InferenceConstraints,
        weights: ObjectiveWeights,
    ) -> SystemDecision {
        let (eligible, unavailable_nodes) = self.capacity.evaluate(requirement);
        let eligible_ids = eligible
            .iter()
            .map(|node| node.id.as_str())
            .collect::<BTreeSet<_>>();
        let unavailable_by_id = unavailable_nodes
            .iter()
            .map(|(id, reason)| (id.as_str(), *reason))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut ordered = candidates.to_vec();
        ordered.sort_by(candidate_order);

        let mut rejected = Vec::new();
        let mut feasible = Vec::new();
        for candidate in ordered {
            let rejection = if !valid_identifier(&candidate.backend_id)
                || !valid_identifier(&candidate.compute_node_id)
            {
                Some(PlacementRejection::InvalidCandidate)
            } else if candidate.estimate.model != requirement.model {
                Some(PlacementRejection::ModelMismatch)
            } else if !eligible_ids.contains(candidate.compute_node_id.as_str()) {
                Some(
                    unavailable_by_id
                        .get(candidate.compute_node_id.as_str())
                        .copied()
                        .map_or(
                            PlacementRejection::UnknownComputeNode,
                            PlacementRejection::Capacity,
                        ),
                )
            } else {
                match self.backends.supports(
                    &candidate.backend_id,
                    &candidate.estimate.model,
                    candidate.estimate.strategy,
                ) {
                    Ok(true) => None,
                    Ok(false) => Some(PlacementRejection::UnsupportedBackendCapability),
                    Err(BackendRegistryError::UnknownBackend(_)) => {
                        Some(PlacementRejection::UnknownBackend)
                    }
                    Err(_) => Some(PlacementRejection::InvalidCandidate),
                }
            };
            if let Some(reason) = rejection {
                rejected.push((candidate, reason));
            } else {
                feasible.push(candidate);
            }
        }

        let estimates = feasible
            .iter()
            .map(|candidate| candidate.estimate.clone())
            .collect::<Vec<_>>();
        let decision = match try_decide(&estimates, constraints, weights) {
            Ok(decision) => decision,
            Err(error) => {
                rejected.extend(feasible.into_iter().map(|candidate| {
                    (
                        candidate,
                        PlacementRejection::Inference(RejectionReason::InvalidPlanningConfig),
                    )
                }));
                sort_rejections(&mut rejected);
                return SystemDecision {
                    selected: None,
                    rejected,
                    unavailable_nodes,
                    config_error: Some(error),
                };
            }
        };

        for (estimate, reason) in &decision.rejected {
            rejected.extend(
                feasible
                    .iter()
                    .filter(|candidate| candidate.estimate == *estimate)
                    .cloned()
                    .map(|candidate| (candidate, PlacementRejection::Inference(*reason))),
            );
        }
        let selected = decision.selected.and_then(|scored| {
            feasible
                .iter()
                .find(|candidate| candidate.estimate == scored.candidate)
                .map(|candidate| PlacementSelection {
                    backend_id: candidate.backend_id.clone(),
                    compute_node_id: candidate.compute_node_id.clone(),
                    scored,
                    fallback: decision.fallback,
                })
        });
        sort_rejections(&mut rejected);
        SystemDecision {
            selected,
            rejected,
            unavailable_nodes,
            config_error: None,
        }
    }
}

fn sort_rejections(values: &mut [(PlacementCandidate, PlacementRejection)]) {
    values.sort_by(|left, right| {
        candidate_order(&left.0, &right.0).then_with(|| left.1.cmp(&right.1))
    });
}

fn candidate_order(left: &PlacementCandidate, right: &PlacementCandidate) -> std::cmp::Ordering {
    left.backend_id
        .cmp(&right.backend_id)
        .then_with(|| left.compute_node_id.cmp(&right.compute_node_id))
        .then_with(|| left.estimate.model.cmp(&right.estimate.model))
        .then_with(|| left.estimate.strategy.cmp(&right.estimate.strategy))
        .then_with(|| {
            left.estimate
                .latency_ms
                .total_cmp(&right.estimate.latency_ms)
        })
        .then_with(|| left.estimate.cost.total_cmp(&right.estimate.cost))
        .then_with(|| {
            left.estimate
                .failure_rate
                .total_cmp(&right.estimate.failure_rate)
        })
        .then_with(|| left.estimate.quality.total_cmp(&right.estimate.quality))
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
    use crate::backend::{BackendResult, MockBackend};
    use crate::backend_registry::BackendDescriptor;
    use crate::capacity::ComputeNode;
    use crate::kv::{KvStrategy, ModelReference};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    fn model() -> ModelReference {
        ModelReference::new("model", "1")
    }

    fn estimate(latency_ms: f64, quality: f64) -> InferenceCandidate {
        InferenceCandidate {
            model: model(),
            strategy: KvStrategy::None,
            latency_ms,
            cost: 0.1,
            failure_rate: 0.01,
            quality,
        }
    }

    fn node(id: &str, healthy: bool) -> ComputeNode {
        ComputeNode {
            id: id.into(),
            region: "region-a".into(),
            accelerator: "simulated".into(),
            total_memory_mb: 100,
            available_memory_mb: 50,
            utilization: 0.5,
            active_requests: 0,
            queue_depth: 0,
            healthy,
            resident_models: BTreeSet::from([model()]),
        }
    }

    fn setup() -> (BackendRegistry, CapacitySnapshot, CapacityRequirement) {
        let mut registry = BackendRegistry::default();
        registry
            .register(
                BackendDescriptor {
                    id: "good".into(),
                    version: "1".into(),
                    capabilities: BTreeMap::from([(model(), BTreeSet::from([KvStrategy::None]))]),
                },
                Arc::new(MockBackend {
                    name: "good".into(),
                    model: model(),
                    supported: vec![KvStrategy::None],
                    result: BackendResult {
                        backend: "good".into(),
                        success: true,
                        latency_ms: Some(1),
                        estimated_cost: Some(0.1),
                        failure: None,
                    },
                }),
            )
            .unwrap();
        let mut capacity = CapacitySnapshot::default();
        capacity.register(node("healthy", true)).unwrap();
        capacity.register(node("down", false)).unwrap();
        let requirement = CapacityRequirement {
            model: model(),
            region: Some("region-a".into()),
            min_available_memory_mb: 10,
            max_queue_depth: 1,
        };
        (registry, capacity, requirement)
    }

    #[test]
    fn only_a_capacity_eligible_declared_backend_can_be_selected() {
        let (registry, capacity, requirement) = setup();
        let planner = SystemPlanner::new(&registry, &capacity);
        let decision = planner.decide(
            &[
                PlacementCandidate {
                    backend_id: "missing".into(),
                    compute_node_id: "healthy".into(),
                    estimate: estimate(1.0, 1.0),
                },
                PlacementCandidate {
                    backend_id: "good".into(),
                    compute_node_id: "down".into(),
                    estimate: estimate(2.0, 1.0),
                },
                PlacementCandidate {
                    backend_id: "good".into(),
                    compute_node_id: "healthy".into(),
                    estimate: estimate(3.0, 1.0),
                },
            ],
            &requirement,
            InferenceConstraints::default(),
            ObjectiveWeights::default(),
        );
        let selected = decision.selected.expect("one valid placement");
        assert_eq!(selected.backend_id, "good");
        assert_eq!(selected.compute_node_id, "healthy");
        assert!(
            decision
                .rejected
                .iter()
                .any(|(_, reason)| { *reason == PlacementRejection::UnknownBackend })
        );
        assert!(decision.rejected.iter().any(|(_, reason)| {
            *reason == PlacementRejection::Capacity(CapacityRejection::Unhealthy)
        }));
    }

    #[test]
    fn invalid_scoring_configuration_fails_closed_after_feasibility() {
        let (registry, capacity, requirement) = setup();
        let decision = SystemPlanner::new(&registry, &capacity).decide(
            &[PlacementCandidate {
                backend_id: "good".into(),
                compute_node_id: "healthy".into(),
                estimate: estimate(3.0, 1.0),
            }],
            &requirement,
            InferenceConstraints {
                max_cost: -1.0,
                ..InferenceConstraints::default()
            },
            ObjectiveWeights::default(),
        );
        assert_eq!(decision.selected, None);
        assert_eq!(
            decision.config_error,
            Some(DecisionError::InvalidConstraints)
        );
        assert_eq!(
            decision.rejected[0].1,
            PlacementRejection::Inference(RejectionReason::InvalidPlanningConfig)
        );
    }

    #[test]
    fn candidate_order_does_not_change_the_decision() {
        let (registry, capacity, requirement) = setup();
        let candidates = vec![
            PlacementCandidate {
                backend_id: "good".into(),
                compute_node_id: "healthy".into(),
                estimate: estimate(10.0, 1.0),
            },
            PlacementCandidate {
                backend_id: "good".into(),
                compute_node_id: "healthy".into(),
                estimate: estimate(20.0, 1.0),
            },
        ];
        let planner = SystemPlanner::new(&registry, &capacity);
        let first = planner.decide(
            &candidates,
            &requirement,
            InferenceConstraints::default(),
            ObjectiveWeights::default(),
        );
        let mut reversed = candidates;
        reversed.reverse();
        let second = planner.decide(
            &reversed,
            &requirement,
            InferenceConstraints::default(),
            ObjectiveWeights::default(),
        );
        assert_eq!(first, second);
    }
}
