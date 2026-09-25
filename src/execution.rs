//! Safe execution handoff from a selected backend to the health-monitor path.
//!
//! The coordinator is backend-agnostic. A registered adapter may be a real
//! provider integration, but this layer verifies its result before recording
//! health so malformed adapter output cannot corrupt circuit-breaker inputs.

use crate::backend::{BackendRequest, BackendResult};
use crate::backend_registry::{BackendRegistry, BackendRegistryError};
use crate::health_monitor::{HealthMonitor, ToolResult};
use crate::kv::{KvStrategy, ModelReference};

/// The identity and selected backend binding for one observed execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRequest {
    pub request_id: String,
    pub tenant: String,
    pub node: String,
    pub backend_id: String,
    pub model: ModelReference,
    pub strategy: KvStrategy,
}

/// An execution failure that is safe to return to an adapter caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    InvalidRequest,
    Backend(BackendRegistryError),
    MissingLatency,
    BackendIdentityMismatch,
    InconsistentBackendResult,
    HealthMonitorClosed,
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest => write!(formatter, "execution request is invalid"),
            Self::Backend(error) => write!(formatter, "backend execution rejected: {error}"),
            Self::MissingLatency => write!(formatter, "backend result omitted observed latency"),
            Self::BackendIdentityMismatch => {
                write!(formatter, "backend result identity did not match request")
            }
            Self::InconsistentBackendResult => write!(
                formatter,
                "backend result has inconsistent success and failure fields"
            ),
            Self::HealthMonitorClosed => write!(formatter, "health monitor is unavailable"),
        }
    }
}

impl std::error::Error for ExecutionError {}

/// Executes through a declared backend and records exactly one validated health outcome.
pub struct ExecutionCoordinator<'a> {
    backends: &'a BackendRegistry,
    monitor: HealthMonitor,
}

impl<'a> ExecutionCoordinator<'a> {
    pub fn new(backends: &'a BackendRegistry, monitor: HealthMonitor) -> Self {
        Self { backends, monitor }
    }

    /// Executes a selected binding only after validating all public identifiers.
    ///
    /// Adapter results must identify the selected backend, carry an observed
    /// latency, and consistently pair `success` with an optional failure value.
    pub async fn execute(
        &self,
        request: &ExecutionRequest,
    ) -> Result<BackendResult, ExecutionError> {
        if ![
            &request.request_id,
            &request.tenant,
            &request.node,
            &request.backend_id,
            &request.model.model_id,
            &request.model.model_version,
        ]
        .into_iter()
        .all(|value| valid_identifier(value))
        {
            return Err(ExecutionError::InvalidRequest);
        }
        let result = self
            .backends
            .execute(
                &request.backend_id,
                &BackendRequest {
                    request_id: request.request_id.clone(),
                    model: request.model.clone(),
                    strategy: request.strategy,
                },
            )
            .map_err(ExecutionError::Backend)?;
        if result.backend != request.backend_id {
            return Err(ExecutionError::BackendIdentityMismatch);
        }
        if result.success == result.failure.is_some() {
            return Err(ExecutionError::InconsistentBackendResult);
        }
        let latency_ms = result.latency_ms.ok_or(ExecutionError::MissingLatency)?;
        self.monitor
            .report(ToolResult {
                tenant: request.tenant.clone(),
                node: request.node.clone(),
                success: result.success,
                latency_ms,
            })
            .await
            .map_err(|_| ExecutionError::HealthMonitorClosed)?;
        Ok(result)
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
    use crate::backend::{BackendFailure, MockBackend};
    use crate::backend_registry::BackendDescriptor;
    use crate::{Edge, NodeState, Router};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    fn model() -> ModelReference {
        ModelReference::new("model", "1")
    }

    fn request() -> ExecutionRequest {
        ExecutionRequest {
            request_id: "req-1".into(),
            tenant: "tenant".into(),
            node: "tool".into(),
            backend_id: "backend".into(),
            model: model(),
            strategy: KvStrategy::None,
        }
    }

    fn registry(result: BackendResult) -> BackendRegistry {
        let mut registry = BackendRegistry::default();
        registry
            .register(
                BackendDescriptor {
                    id: "backend".into(),
                    version: "1".into(),
                    capabilities: BTreeMap::from([(model(), BTreeSet::from([KvStrategy::None]))]),
                },
                Arc::new(MockBackend {
                    name: "backend".into(),
                    model: model(),
                    supported: vec![KvStrategy::None],
                    result,
                }),
            )
            .unwrap();
        registry
    }

    fn router() -> Router {
        let router = Router::new();
        router.add_tenant("tenant");
        router.add_edge("tenant", Edge::new("start", "tool"));
        router.add_edge("tenant", Edge::new("tool", "done"));
        router
    }

    #[tokio::test]
    async fn valid_execution_is_recorded_through_the_health_monitor() {
        let result = BackendResult {
            backend: "backend".into(),
            success: true,
            latency_ms: Some(7),
            estimated_cost: Some(0.1),
            failure: None,
        };
        let registry = registry(result.clone());
        let router = router();
        let (monitor, task) = HealthMonitor::start(router.clone(), 1);
        let coordinator = ExecutionCoordinator::new(&registry, monitor.clone());
        assert_eq!(coordinator.execute(&request()).await.unwrap(), result);
        drop(coordinator);
        drop(monitor);
        task.await.unwrap();
        assert_eq!(
            router.node_state("tenant", "tool"),
            Some(NodeState::Healthy)
        );
    }

    #[tokio::test]
    async fn malformed_or_inconsistent_results_do_not_reach_health() {
        let invalid = ExecutionRequest {
            request_id: "bad\nrequest".into(),
            ..request()
        };
        let result = BackendResult {
            backend: "backend".into(),
            success: true,
            latency_ms: Some(7),
            estimated_cost: None,
            failure: Some(BackendFailure::Timeout),
        };
        let registry = registry(result);
        let router = router();
        let (monitor, task) = HealthMonitor::start(router.clone(), 1);
        let coordinator = ExecutionCoordinator::new(&registry, monitor.clone());
        assert_eq!(
            coordinator.execute(&invalid).await,
            Err(ExecutionError::InvalidRequest)
        );
        assert_eq!(
            coordinator.execute(&request()).await,
            Err(ExecutionError::InconsistentBackendResult)
        );
        drop(coordinator);
        drop(monitor);
        task.await.unwrap();
        assert_eq!(router.node_state("tenant", "tool"), None);
    }
}
