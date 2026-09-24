//! Offline deterministic evaluation helpers. No live backend is invoked.
use crate::inference::{
    DecisionExplanation, InferenceCandidate, InferenceConstraints, ObjectiveWeights, decide,
};
pub fn replay(
    candidates: Vec<InferenceCandidate>,
    constraints: InferenceConstraints,
    weights: ObjectiveWeights,
) -> DecisionExplanation {
    decide(candidates, constraints, weights)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::{KvStrategy, ModelReference};
    #[test]
    fn replay_is_deterministic() {
        let c = InferenceCandidate {
            model: ModelReference::new("m", "1"),
            strategy: KvStrategy::None,
            latency_ms: 1.0,
            cost: 0.0,
            failure_rate: 0.0,
            quality: 1.0,
        };
        assert_eq!(
            replay(vec![c.clone()], Default::default(), Default::default()),
            replay(vec![c], Default::default(), Default::default())
        );
    }
}
