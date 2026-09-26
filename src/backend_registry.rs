//! Deterministic registry for declared inference backend capabilities.
use crate::backend::{BackendRequest, BackendResult, InferenceBackend};
use crate::kv::{KvStrategy, ModelReference};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendDescriptor {
    pub id: String,
    pub version: String,
    pub capabilities: BTreeMap<ModelReference, BTreeSet<KvStrategy>>,
}
impl BackendDescriptor {
    pub fn supports(&self, model: &ModelReference, strategy: KvStrategy) -> bool {
        self.capabilities
            .get(model)
            .is_some_and(|strategies| strategies.contains(&strategy))
    }
    fn validate(&self) -> Result<(), BackendRegistryError> {
        if !identifier(&self.id) || !identifier(&self.version) || self.capabilities.is_empty() {
            return Err(BackendRegistryError::InvalidDescriptor);
        }
        if self.capabilities.iter().any(|(model, strategies)| {
            !identifier(&model.model_id)
                || !identifier(&model.model_version)
                || strategies.is_empty()
        }) {
            return Err(BackendRegistryError::InvalidDescriptor);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendRegistryError {
    InvalidDescriptor,
    DuplicateBackend(String),
    UnknownBackend(String),
    UnsupportedCapability {
        backend: String,
        model: ModelReference,
        strategy: KvStrategy,
    },
}
impl std::fmt::Display for BackendRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for BackendRegistryError {}

#[derive(Clone, Default)]
pub struct BackendRegistry {
    entries: BTreeMap<String, RegisteredBackend>,
    revision: u64,
}
#[derive(Clone)]
struct RegisteredBackend {
    descriptor: BackendDescriptor,
    backend: Arc<dyn InferenceBackend>,
}

impl BackendRegistry {
    pub fn register(
        &mut self,
        descriptor: BackendDescriptor,
        backend: Arc<dyn InferenceBackend>,
    ) -> Result<(), BackendRegistryError> {
        descriptor.validate()?;
        if descriptor.id != backend.name() {
            return Err(BackendRegistryError::InvalidDescriptor);
        }
        if self.entries.contains_key(&descriptor.id) {
            return Err(BackendRegistryError::DuplicateBackend(descriptor.id));
        }
        self.entries.insert(
            descriptor.id.clone(),
            RegisteredBackend {
                descriptor,
                backend,
            },
        );
        self.revision += 1;
        Ok(())
    }
    /// Checks a declared capability without executing a backend.
    ///
    /// Planning uses this gate before selection so a selected strategy cannot
    /// reach an adapter that never advertised support for it.
    pub fn supports(
        &self,
        backend_id: &str,
        model: &ModelReference,
        strategy: KvStrategy,
    ) -> Result<bool, BackendRegistryError> {
        let entry = self
            .entries
            .get(backend_id)
            .ok_or_else(|| BackendRegistryError::UnknownBackend(backend_id.into()))?;
        Ok(entry.descriptor.supports(model, strategy))
    }
    pub fn execute(
        &self,
        backend_id: &str,
        request: &BackendRequest,
    ) -> Result<BackendResult, BackendRegistryError> {
        let entry = self
            .entries
            .get(backend_id)
            .ok_or_else(|| BackendRegistryError::UnknownBackend(backend_id.into()))?;
        if !entry.descriptor.supports(&request.model, request.strategy) {
            return Err(BackendRegistryError::UnsupportedCapability {
                backend: backend_id.into(),
                model: request.model.clone(),
                strategy: request.strategy,
            });
        }
        Ok(entry.backend.execute(request))
    }
    pub fn backend_ids(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendFailure, MockBackend};
    fn setup() -> (BackendRegistry, BackendRequest) {
        let model = ModelReference::new("mock-model", "1");
        let request = BackendRequest {
            request_id: "req-1".into(),
            model: model.clone(),
            strategy: KvStrategy::None,
        };
        let backend = Arc::new(MockBackend {
            name: "mock".into(),
            model: model.clone(),
            supported: vec![KvStrategy::None],
            result: BackendResult {
                backend: "mock".into(),
                success: true,
                latency_ms: Some(1),
                estimated_cost: None,
                failure: None,
            },
        });
        let mut registry = BackendRegistry::default();
        registry
            .register(
                BackendDescriptor {
                    id: "mock".into(),
                    version: "1".into(),
                    capabilities: BTreeMap::from([(model, BTreeSet::from([KvStrategy::None]))]),
                },
                backend,
            )
            .unwrap();
        (registry, request)
    }
    #[test]
    fn registry_validates_capability_before_execution() {
        let (registry, request) = setup();
        assert!(registry.execute("mock", &request).unwrap().success);
        let unsupported = BackendRequest {
            strategy: KvStrategy::PrefixCache,
            ..request
        };
        assert!(matches!(
            registry.execute("mock", &unsupported),
            Err(BackendRegistryError::UnsupportedCapability { .. })
        ));
    }
    #[test]
    fn supports_checks_declarations_without_executing() {
        let (registry, request) = setup();
        assert!(
            registry
                .supports("mock", &request.model, KvStrategy::None)
                .unwrap()
        );
        assert!(
            !registry
                .supports("mock", &request.model, KvStrategy::PrefixCache)
                .unwrap()
        );
    }
    #[test]
    fn registry_is_ordered_and_rejects_duplicates() {
        let (mut registry, request) = setup();
        let duplicate = Arc::new(MockBackend {
            name: "mock".into(),
            model: request.model.clone(),
            supported: vec![KvStrategy::None],
            result: BackendResult {
                backend: "mock".into(),
                success: false,
                latency_ms: None,
                estimated_cost: None,
                failure: Some(BackendFailure::Overloaded),
            },
        });
        let descriptor = BackendDescriptor {
            id: "mock".into(),
            version: "2".into(),
            capabilities: BTreeMap::from([(request.model, BTreeSet::from([KvStrategy::None]))]),
        };
        assert!(matches!(
            registry.register(descriptor, duplicate),
            Err(BackendRegistryError::DuplicateBackend(_))
        ));
        assert_eq!(registry.backend_ids(), vec!["mock"]);
    }
}
