//! Bounded recovery plans for explicit routing escalations.
use crate::{EscalationContext, NodeId};

/// A deterministic, bounded recovery suggestion for an LLM or human operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPlan {
    pub blocked_nodes: Vec<NodeId>,
    pub steps: Vec<String>,
}

/// Converts an escalation into a bounded recovery plan.
pub trait EscalationAdapter: Send + Sync {
    fn plan(&self, context: &EscalationContext) -> RecoveryPlan;
}

/// Default adapter that returns deterministic recovery actions without a network call.
#[derive(Debug, Clone)]
pub struct BoundedRecoveryAdapter {
    max_steps: usize,
}

impl BoundedRecoveryAdapter {
    pub fn new(max_steps: usize) -> Self {
        Self {
            max_steps: max_steps.max(1),
        }
    }
}

impl EscalationAdapter for BoundedRecoveryAdapter {
    fn plan(&self, context: &EscalationContext) -> RecoveryPlan {
        let mut steps = context
            .failed_or_blocked_nodes
            .iter()
            .take(self.max_steps)
            .map(|node| format!("Inspect or replace blocked tool: {node}"))
            .collect::<Vec<_>>();
        if steps.len() < self.max_steps {
            steps.push("Ask the recovery planner for a bounded alternate workflow.".to_string());
        }
        steps.truncate(self.max_steps);
        RecoveryPlan {
            blocked_nodes: context.failed_or_blocked_nodes.clone(),
            steps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EscalationReason, TenantId};

    #[test]
    fn recovery_plan_is_bounded() {
        let plan = BoundedRecoveryAdapter::new(2).plan(&EscalationContext {
            tenant_id: TenantId::from("acme"),
            start: "start".into(),
            goal: "done".into(),
            failed_or_blocked_nodes: vec!["a".into(), "b".into(), "c".into()],
            reason: EscalationReason::NoFeasiblePath,
        });
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.blocked_nodes.len(), 3);
    }
}
