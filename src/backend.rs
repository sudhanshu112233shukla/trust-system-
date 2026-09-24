//! Generic inference backend boundary and deterministic test-only mock backend.
use crate::kv::{KvStrategy, ModelReference};
#[derive(Debug, Clone, PartialEq)]
pub struct BackendRequest {
    pub request_id: String,
    pub model: ModelReference,
    pub strategy: KvStrategy,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendFailure {
    Timeout,
    Overloaded,
    OutOfMemory,
    UnavailableModel,
    KvTransferFailed,
    CapacityExhausted,
    QualityViolation,
}
#[derive(Debug, Clone, PartialEq)]
pub struct BackendResult {
    pub backend: String,
    pub success: bool,
    pub latency_ms: Option<u64>,
    pub estimated_cost: Option<f64>,
    pub failure: Option<BackendFailure>,
}
pub trait InferenceBackend: Send + Sync {
    fn name(&self) -> &str;
    fn supports(&self, model: &ModelReference, strategy: KvStrategy) -> bool;
    fn execute(&self, request: &BackendRequest) -> BackendResult;
}
#[derive(Debug, Clone)]
pub struct MockBackend {
    pub name: String,
    pub model: ModelReference,
    pub supported: Vec<KvStrategy>,
    pub result: BackendResult,
}
impl InferenceBackend for MockBackend {
    fn name(&self) -> &str {
        &self.name
    }
    fn supports(&self, m: &ModelReference, s: KvStrategy) -> bool {
        &self.model == m && self.supported.contains(&s)
    }
    fn execute(&self, r: &BackendRequest) -> BackendResult {
        if !self.supports(&r.model, r.strategy) {
            return BackendResult {
                backend: self.name.clone(),
                success: false,
                latency_ms: None,
                estimated_cost: None,
                failure: Some(BackendFailure::UnavailableModel),
            };
        }
        self.result.clone()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mock_never_claims_an_unsupported_strategy() {
        let m = ModelReference::new("mock", "1");
        let b = MockBackend {
            name: "mock".into(),
            model: m.clone(),
            supported: vec![KvStrategy::None],
            result: BackendResult {
                backend: "mock".into(),
                success: true,
                latency_ms: Some(1),
                estimated_cost: None,
                failure: None,
            },
        };
        let r = b.execute(&BackendRequest {
            request_id: "r".into(),
            model: m,
            strategy: KvStrategy::PrefixCache,
        });
        assert_eq!(r.failure, Some(BackendFailure::UnavailableModel));
    }
}
