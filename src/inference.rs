//! Deterministic inference candidate feasibility, scoring, and bounded fallbacks.
use crate::kv::{KvStrategy, ModelReference};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    InvalidEstimate,
    LatencyConstraint,
    CostConstraint,
    QualityConstraint,
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
}
fn valid(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}
pub fn decide(
    mut candidates: Vec<InferenceCandidate>,
    c: InferenceConstraints,
    w: ObjectiveWeights,
) -> DecisionExplanation {
    let mut rejected = Vec::new();
    let mut feasible = Vec::new();
    for candidate in candidates.drain(..) {
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
        } else if candidate.latency_ms > c.max_latency_ms {
            Some(RejectionReason::LatencyConstraint)
        } else if candidate.cost > c.max_cost {
            Some(RejectionReason::CostConstraint)
        } else if candidate.quality < c.min_quality {
            Some(RejectionReason::QualityConstraint)
        } else {
            None
        };
        if let Some(reason) = reason {
            rejected.push((candidate, reason))
        } else {
            let score = w.latency * candidate.latency_ms
                + w.cost * candidate.cost
                + w.reliability * candidate.failure_rate
                + w.quality * (1.0 - candidate.quality);
            feasible.push(ScoredCandidate { candidate, score })
        }
    }
    feasible.sort_by(|a, b| {
        a.score
            .total_cmp(&b.score)
            .then_with(|| a.candidate.model.cmp(&b.candidate.model))
            .then_with(|| a.candidate.strategy.cmp(&b.candidate.strategy))
    });
    let selected = feasible.into_iter().next();
    DecisionExplanation {
        fallback: selected
            .as_ref()
            .map_or_else(Vec::new, |s| fallback(s.candidate.strategy)),
        selected,
        rejected,
    }
}
fn fallback(s: KvStrategy) -> Vec<KvStrategy> {
    match s {
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
    fn c(id: &str, lat: f64, cost: f64, q: f64) -> InferenceCandidate {
        InferenceCandidate {
            model: ModelReference::new(id, "1"),
            strategy: KvStrategy::PrefixCache,
            latency_ms: lat,
            cost,
            failure_rate: 0.0,
            quality: q,
        }
    }
    #[test]
    fn constraints_precede_scoring_and_ties_are_stable() {
        let d = decide(
            vec![
                c("cheap-bad", 1., 0., 0.5),
                c("b", 10., 0.1, 0.99),
                c("a", 10., 0.1, 0.99),
            ],
            InferenceConstraints {
                min_quality: 0.9,
                ..Default::default()
            },
            Default::default(),
        );
        assert_eq!(d.selected.unwrap().candidate.model.model_id, "a");
        assert_eq!(d.rejected.len(), 1)
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
