//! Async ingestion for real tool/API outcomes.
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{NodeId, Router, TenantId};

/// A completed tool invocation observed by an SDK, worker, or sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub tenant: TenantId,
    pub node: NodeId,
    pub success: bool,
    pub latency_ms: u64,
}

/// Sends observed tool results to a background task without coupling callers to
/// router locking or health-state transitions.
#[derive(Clone)]
pub struct HealthMonitor {
    sender: mpsc::Sender<ToolResult>,
}

impl HealthMonitor {
    /// Starts a bounded async consumer that applies every accepted result through
    /// Router::record_result, the single health-state transition path.
    pub fn start(router: Router, capacity: usize) -> (Self, JoinHandle<()>) {
        let (sender, mut receiver) = mpsc::channel::<ToolResult>(capacity);
        let task = tokio::spawn(async move {
            while let Some(result) = receiver.recv().await {
                router.record_result(
                    &result.tenant,
                    &result.node,
                    result.success,
                    result.latency_ms,
                );
            }
        });
        (Self { sender }, task)
    }

    /// Queues an observed outcome with explicit bounded backpressure.
    pub async fn report(
        &self,
        result: ToolResult,
    ) -> Result<(), mpsc::error::SendError<ToolResult>> {
        self.sender.send(result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edge, NodeState, OPEN_THRESHOLD};

    #[tokio::test]
    async fn monitor_applies_real_results_through_router_health_path() {
        let router = Router::new();
        router.add_tenant("monitor");
        router.add_edge("monitor", Edge::new("start", "tool"));
        router.add_edge("monitor", Edge::new("tool", "done"));
        let (monitor, task) = HealthMonitor::start(router.clone(), 8);
        for _ in 0..OPEN_THRESHOLD {
            monitor
                .report(ToolResult {
                    tenant: "monitor".into(),
                    node: "tool".into(),
                    success: false,
                    latency_ms: 30_000,
                })
                .await
                .unwrap();
        }
        drop(monitor);
        task.await.unwrap();
        assert_eq!(router.node_state("monitor", "tool"), Some(NodeState::Open));
    }
}
