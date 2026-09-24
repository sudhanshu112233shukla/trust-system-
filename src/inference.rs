//! Deterministic inference feasibility, normalized scoring, and bounded fallbacks.
use crate::kv::{KvStrategy, ModelReference};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct InferenceCandidate {
    pub model: ModelReference,
    pub strategy: KvStrategy,
    pub latency_ms: f64,
    pub cost: f64,
    pub failure_rate: f64,
    pub quality: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InferenceConstraints {
    pub max_latency_ms: f64,
    pub max_cost: f64,
    pub min_quality: f64,
}
impl Default for InferenceConstraints {
    fn default() -> Self {
        Self {
            max_latency_ms: 1000.0,
            max_cost: 1.0,
            min_quality: 0.0,
        }
    }
}
impl InferenceConstraints {
    pub fn validate(self) -> Result<(), DecisionError> {
        if !self.max_latency_ms.is_finite()
            || self.max_latency_ms < 0.0
            || !self.max_cost.is_finite()
            || self.max_cost < 0.0
            || !self.min_quality.is_finite()
            || !(0.0..=1.0).contains(&self.min_quality)
        {
            Err(DecisionError::InvalidConstraints)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectiveWeights {
    pub latency: f64,
    pub cost: f64,
    pub reliability: f64,
    pub quality: f64,
}
impl Default for ObjectiveWeights {
    fn default() -> Self {
        Self {
            latency: 1.0,
            cost: 1.0,
            reliability: 1.0,
            quality: 1.0,
        }
    }
}
impl ObjectiveWeights {
    pub fn validate(self) -> Result<(), DecisionError> {
        let all = [self.latency, self.cost, self.reliability, self.quality];
        if all.iter().any(|v| !v.is_finite() || *v < 0.0) || all.iter().all(|v| *v == 0.0) {
            Err(DecisionError::InvalidObjective)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionError {
    InvalidConstraints,
    InvalidObjective,
}
impl fmt::Display for DecisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for DecisionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RejectionReason {
    InvalidEstimate,
    LatencyConstraint,
    CostConstraint,
    QualityConstraint,
    InvalidPlanningConfig,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredCandidate {
    pub candidate: InferenceCandidate,
    pub score: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionExplanation {
    pub selected: Option<ScoredCandidate>,
    pub rejected: Vec<(InferenceCandidate, RejectionReason)>,
    pub fallback: Vec<KvStrategy>,
    pub config_error: Option<DecisionError>,
}

fn valid(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}
fn normalized(value: f64, ceiling: f64) -> f64 {
    if ceiling == 0.0 { 0.0 } else { value / ceiling }
}
fn candidate_order(a: &InferenceCandidate, b: &InferenceCandidate) -> std::cmp::Ordering {
    a.model
        .cmp(&b.model)
        .then_with(|| a.strategy.cmp(&b.strategy))
        .then_with(|| a.latency_ms.total_cmp(&b.latency_ms))
        .then_with(|| a.cost.total_cmp(&b.cost))
        .then_with(|| a.failure_rate.total_cmp(&b.failure_rate))
        .then_with(|| a.quality.total_cmp(&b.quality))
}

pub fn try_decide(
    candidates: &[InferenceCandidate],
    constraints: InferenceConstraints,
    weights: ObjectiveWeights,
) -> Result<DecisionExplanation, DecisionError> {
    constraints.validate()?;
    weights.validate()?;
    let mut rejected = Vec::new();
    let mut feasible = Vec::new();
    for candidate in candidates {
        let reason = if ![
            candidate.latency_ms,
            candidate.cost,
            candidate.failure_rate,
            candidate.quality,
        ]
        .into_iter()
        .all(valid)
            || candidate.failure_rate > 1.0
            || candidate.quality > 1.0
        {
            Some(RejectionReason::InvalidEstimate)
        } else if candidate.latency_ms > constraints.max_latency_ms {
            Some(RejectionReason::LatencyConstraint)
        } else if candidate.cost > constraints.max_cost {
            Some(RejectionReason::CostConstraint)
        } else if candidate.quality < constraints.min_quality {
            Some(RejectionReason::QualityConstraint)
        } else {
            None
        };
        if let Some(reason) = reason {
            rejected.push((candidate.clone(), reason));
        } else {
            let score = weights.latency
                * normalized(candidate.latency_ms, constraints.max_latency_ms)
                + weights.cost * normalized(candidate.cost, constraints.max_cost)
                + weights.reliability * candidate.failure_rate
                + weights.quality * (1.0 - candidate.quality);
            feasible.push(ScoredCandidate {
                candidate: candidate.clone(),
                score,
            });
        }
    }
    rejected.sort_by(|a, b| candidate_order(&a.0, &b.0).then_with(|| a.1.cmp(&b.1)));
    feasible.sort_by(|a, b| {
        a.score
            .total_cmp(&b.score)
            .then_with(|| candidate_order(&a.candidate, &b.candidate))
    });
    let selected = feasible.into_iter().next();
    Ok(DecisionExplanation {
        fallback: selected
            .as_ref()
            .map_or_else(Vec::new, |value| fallback(value.candidate.strategy)),
        selected,
        rejected,
        config_error: None,
    })
}

/// Backward-compatible decision entry point. Invalid configuration fails closed.
pub fn decide(
    candidates: Vec<InferenceCandidate>,
    constraints: InferenceConstraints,
    weights: ObjectiveWeights,
) -> DecisionExplanation {
    match try_decide(&candidates, constraints, weights) {
        Ok(decision) => decision,
        Err(error) => {
            let mut rejected = candidates
                .into_iter()
                .map(|candidate| (candidate, RejectionReason::InvalidPlanningConfig))
                .collect::<Vec<_>>();
            rejected.sort_by(|a, b| candidate_order(&a.0, &b.0));
            DecisionExplanation {
                selected: None,
                rejected,
                fallback: Vec::new(),
                config_error: Some(error),
            }
        }
    }
}

fn fallback(strategy: KvStrategy) -> Vec<KvStrategy> {
    match strategy {
        KvStrategy::CrossModelKvTransfer => vec![
            KvStrategy::CachedKv,
            KvStrategy::PrefixCache,
            KvStrategy::None,
        ],
        KvStrategy::CachedKv => vec![KvStrategy::PrefixCache, KvStrategy::None],
        KvStrategy::PrefixCache => vec![KvStrategy::None],
        KvStrategy::None => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(id: &str, latency_ms: f64, cost: f64, quality: f64) -> InferenceCandidate {
        InferenceCandidate {
            model: ModelReference::new(id, "1"),
            strategy: KvStrategy::PrefixCache,
            latency_ms,
            cost,
            failure_rate: 0.0,
            quality,
        }
    }
    #[test]
    fn constraints_precede_scoring_and_ties_are_stable() {
        let decision = decide(
            vec![
                candidate("cheap-bad", 1.0, 0.0, 0.5),
                candidate("b", 10.0, 0.1, 0.99),
                candidate("a", 10.0, 0.1, 0.99),
            ],
            InferenceConstraints {
                min_quality: 0.9,
                ..Default::default()
            },
            Default::default(),
        );
        assert_eq!(decision.selected.unwrap().candidate.model.model_id, "a");
        assert_eq!(decision.rejected.len(), 1);
    }
    #[test]
    fn invalid_planning_config_fails_closed() {
        let decision = decide(
            vec![candidate("a", 1.0, 0.0, 1.0)],
            Default::default(),
            ObjectiveWeights {
                latency: f64::NAN,
                ..Default::default()
            },
        );
        assert_eq!(decision.selected, None);
        assert_eq!(decision.config_error, Some(DecisionError::InvalidObjective));
        assert_eq!(
            decision.rejected[0].1,
            RejectionReason::InvalidPlanningConfig
        );
    }
    #[test]
    fn normalization_allows_cost_to_influence_choice() {
        let decision = try_decide(
            &[
                candidate("fast-expensive", 1.0, 1.0, 1.0),
                candidate("slow-cheap", 1000.0, 0.0, 1.0),
            ],
            Default::default(),
            ObjectiveWeights {
                latency: 1.0,
                cost: 1.0,
                reliability: 0.0,
                quality: 0.0,
            },
        )
        .unwrap();
        assert_eq!(
            decision.selected.unwrap().candidate.model.model_id,
            "slow-cheap"
        );
    }
    #[test]
    fn fallbacks_are_bounded() {
        assert_eq!(
            fallback(KvStrategy::CrossModelKvTransfer),
            vec![
                KvStrategy::CachedKv,
                KvStrategy::PrefixCache,
                KvStrategy::None
            ]
        );
    }
}
