//! Deterministic execution planning built on the routing core.
//!
//! The planner deliberately does not execute tools or call an LLM. It converts a
//! validated routing request into a bounded `ExecutionPlan`, or emits a bounded
//! recovery plan when deterministic routing cannot proceed.

use crate::escalation::{BoundedRecoveryAdapter, EscalationAdapter, RecoveryPlan};
use crate::kv::{KvIntelligence, KvPlanMetadata, KvPlanningContext};
use crate::{EscalationContext, NodeId, Route, RouteDecision, Router, TenantId};

pub const MAX_EXECUTION_PLAN_STEPS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningRequest {
    pub tenant_id: TenantId,
    pub start: NodeId,
    pub goal: NodeId,
}

impl PlanningRequest {
    pub fn new(
        tenant_id: impl Into<TenantId>,
        start: impl Into<NodeId>,
        goal: impl Into<NodeId>,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            start: start.into(),
            goal: goal.into(),
        }
    }

    pub fn validate(&self) -> Result<(), PlanningError> {
        validate_identifier("tenant_id", &self.tenant_id)?;
        validate_identifier("start", &self.start)?;
        validate_identifier("goal", &self.goal)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanningError {
    InvalidRequest(String),
    RouteExceedsStepLimit { steps: usize, limit: usize },
}

impl std::fmt::Display for PlanningError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid planning request: {message}")
            }
            Self::RouteExceedsStepLimit { steps, limit } => {
                write!(
                    formatter,
                    "route has {steps} steps, above execution-plan limit {limit}"
                )
            }
        }
    }
}

impl std::error::Error for PlanningError {}

/// Snapshot records the routing inputs and decision facts that generated a plan.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemSnapshot {
    pub tenant_id: TenantId,
    pub start: NodeId,
    pub goal: NodeId,
    pub cache_hit: bool,
}

/// Candidate contains the only feasible deterministic route generated for a request.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub nodes: Vec<NodeId>,
    pub score: f64,
}

/// A tool-oriented step. Adapters map this stable model to concrete backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStep {
    pub ordinal: usize,
    pub node: NodeId,
}

/// Bounded, deterministic work handed to an execution adapter.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionPlan {
    pub schema_version: u8,
    pub snapshot: SystemSnapshot,
    pub candidate: Candidate,
    pub steps: Vec<ExecutionStep>,
    pub kv: KvPlanMetadata,
}

/// Planning always returns something explicit: executable work or bounded recovery.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanOutcome {
    Execute(ExecutionPlan),
    Recover {
        snapshot: SystemSnapshot,
        escalation: EscalationContext,
        recovery: RecoveryPlan,
    },
}

/// Separates request analysis, feasibility, scoring, and selection from execution.
#[derive(Debug, Clone)]
pub struct Planner {
    router: Router,
    max_steps: usize,
    recovery_adapter: BoundedRecoveryAdapter,
    kv: KvIntelligence,
}

impl Planner {
    pub fn new(router: Router) -> Self {
        Self::with_limits(router, MAX_EXECUTION_PLAN_STEPS, 4)
    }

    pub fn with_limits(router: Router, max_steps: usize, max_recovery_steps: usize) -> Self {
        Self {
            router,
            max_steps: max_steps.clamp(1, MAX_EXECUTION_PLAN_STEPS),
            recovery_adapter: BoundedRecoveryAdapter::new(max_recovery_steps),
            kv: KvIntelligence::default(),
        }
    }

    pub fn with_kv_intelligence(mut self, kv: KvIntelligence) -> Self {
        self.kv = kv;
        self
    }

    /// Analyze, generate, validate, score, and select a deterministic execution plan.
    pub fn plan(&self, request: PlanningRequest) -> Result<PlanOutcome, PlanningError> {
        request.validate()?;

        match self
            .router
            .route(&request.tenant_id, &request.start, &request.goal)
        {
            RouteDecision::Routed(route) => self.execution_plan(request, route),
            RouteDecision::Escalate(escalation) => {
                let snapshot = SystemSnapshot {
                    tenant_id: request.tenant_id,
                    start: request.start,
                    goal: request.goal,
                    cache_hit: false,
                };
                let recovery = self.recovery_adapter.plan(&escalation);
                Ok(PlanOutcome::Recover {
                    snapshot,
                    escalation,
                    recovery,
                })
            }
        }
    }

    pub fn plan_with_kv(
        &self,
        request: PlanningRequest,
        context: KvPlanningContext,
    ) -> Result<PlanOutcome, PlanningError> {
        let outcome = self.plan(request)?;
        match outcome {
            PlanOutcome::Execute(mut plan) => {
                plan.kv = self.kv.select(&context);
                Ok(PlanOutcome::Execute(plan))
            }
            recovery => Ok(recovery),
        }
    }

    fn execution_plan(
        &self,
        request: PlanningRequest,
        route: Route,
    ) -> Result<PlanOutcome, PlanningError> {
        if route.nodes.len() > self.max_steps {
            return Err(PlanningError::RouteExceedsStepLimit {
                steps: route.nodes.len(),
                limit: self.max_steps,
            });
        }
        if !route.total_cost.is_finite() || route.total_cost < 0.0 {
            return Err(PlanningError::InvalidRequest(
                "routing core returned an invalid candidate score".into(),
            ));
        }

        let snapshot = SystemSnapshot {
            tenant_id: request.tenant_id,
            start: request.start,
            goal: request.goal,
            cache_hit: route.cache_hit,
        };
        let candidate = Candidate {
            nodes: route.nodes.clone(),
            score: route.total_cost,
        };
        let steps = route
            .nodes
            .iter()
            .enumerate()
            .map(|(ordinal, node)| ExecutionStep {
                ordinal,
                node: node.clone(),
            })
            .collect();

        Ok(PlanOutcome::Execute(ExecutionPlan {
            schema_version: 1,
            snapshot,
            candidate,
            steps,
            kv: self.kv.normal(),
        }))
    }
}

fn validate_identifier(field: &str, value: &str) -> Result<(), PlanningError> {
    if value.is_empty() || value.len() > 128 {
        return Err(PlanningError::InvalidRequest(format!(
            "{field} must be 1-128 characters"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(PlanningError::InvalidRequest(format!(
            "{field} contains invalid characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::thread;

    use crate::{Edge, NodeState, OPEN_THRESHOLD};

    use super::*;

    #[test]
    fn planner_produces_a_bounded_execution_plan() {
        let planner = Planner::new(sample_router());
        let outcome = planner
            .plan(PlanningRequest::new("acme", "start", "done"))
            .expect("valid plan");

        let PlanOutcome::Execute(plan) = outcome else {
            panic!("expected execution plan");
        };
        assert_eq!(plan.schema_version, 1);
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].node, "start");
        assert_eq!(plan.steps[2].node, "done");
        assert!(!plan.snapshot.cache_hit);
    }

    #[test]
    fn planner_reuses_core_cache_without_creating_a_stale_plan_cache() {
        let planner = Planner::new(sample_router());
        let first = planner
            .plan(PlanningRequest::new("acme", "start", "done"))
            .expect("first plan");
        let second = planner
            .plan(PlanningRequest::new("acme", "start", "done"))
            .expect("second plan");

        let PlanOutcome::Execute(first) = first else {
            panic!("expected plan")
        };
        let PlanOutcome::Execute(second) = second else {
            panic!("expected plan")
        };
        assert!(!first.snapshot.cache_hit);
        assert!(second.snapshot.cache_hit);
        assert_eq!(first.candidate.nodes, second.candidate.nodes);
    }

    #[test]
    fn health_change_invalidates_cached_plan_and_selects_fallback() {
        let router = sample_router();
        let planner = Planner::new(router.clone());
        let _ = planner
            .plan(PlanningRequest::new("acme", "start", "done"))
            .expect("initial plan");
        for _ in 0..OPEN_THRESHOLD {
            router.record_result("acme", "primary", false, 30_000);
        }

        let outcome = planner
            .plan(PlanningRequest::new("acme", "start", "done"))
            .expect("fallback plan");
        let PlanOutcome::Execute(plan) = outcome else {
            panic!("expected fallback execution plan");
        };
        assert_eq!(router.node_state("acme", "primary"), Some(NodeState::Open));
        assert!(!plan.snapshot.cache_hit);
        assert!(plan.candidate.nodes.iter().any(|node| node == "fallback"));
        assert!(!plan.candidate.nodes.iter().any(|node| node == "primary"));
    }

    #[test]
    fn planning_failure_returns_bounded_recovery() {
        let router = sample_router();
        let planner = Planner::with_limits(router.clone(), 64, 2);
        for node in ["primary", "fallback"] {
            for _ in 0..OPEN_THRESHOLD {
                router.record_result("acme", node, false, 30_000);
            }
        }

        let outcome = planner
            .plan(PlanningRequest::new("acme", "start", "done"))
            .expect("recovery outcome");
        let PlanOutcome::Recover { recovery, .. } = outcome else {
            panic!("expected recovery outcome");
        };
        assert!(recovery.steps.len() <= 2);
    }

    #[test]
    fn malformed_planning_requests_are_rejected_before_routing() {
        let planner = Planner::new(sample_router());
        let error = planner
            .plan(PlanningRequest::new("acme\nlog-injection", "start", "done"))
            .expect_err("malformed tenant must fail");
        assert!(error.to_string().contains("invalid planning request"));
    }

    #[test]
    fn concurrent_planning_requests_remain_deterministic() {
        let planner = Planner::new(sample_router());
        let mut workers = Vec::new();
        for _ in 0..8 {
            let planner = planner.clone();
            workers.push(thread::spawn(move || {
                for _ in 0..100 {
                    let outcome = planner
                        .plan(PlanningRequest::new("acme", "start", "done"))
                        .expect("plan must succeed");
                    let PlanOutcome::Execute(plan) = outcome else {
                        panic!("unexpected recovery");
                    };
                    assert_eq!(
                        plan.candidate.nodes.first().map(String::as_str),
                        Some("start")
                    );
                    assert_eq!(
                        plan.candidate.nodes.last().map(String::as_str),
                        Some("done")
                    );
                }
            }));
        }
        for worker in workers {
            worker.join().expect("planning worker panicked");
        }
    }

    #[test]
    fn kv_capability_is_selected_only_when_the_model_declares_it() {
        use crate::kv::{KvStrategy, ModelCapabilities, ModelMetadata, ModelReference};
        let mut kv = KvIntelligence::default();
        kv.models
            .register(ModelMetadata {
                model: ModelReference::new("model-a", "1"),
                kv_layout: "layout-v1".into(),
                capabilities: ModelCapabilities::normal_prefill().with_kv(KvStrategy::PrefixCache),
            })
            .unwrap();
        let plan = Planner::new(sample_router())
            .with_kv_intelligence(kv)
            .plan_with_kv(
                PlanningRequest::new("acme", "start", "done"),
                KvPlanningContext {
                    target: ModelReference::new("model-a", "1"),
                    source: None,
                    requested: KvStrategy::PrefixCache,
                },
            )
            .unwrap();
        let PlanOutcome::Execute(plan) = plan else {
            panic!("expected plan")
        };
        assert_eq!(plan.kv.strategy, KvStrategy::PrefixCache);
        assert_eq!(plan.kv.fallback_reason, None);
    }

    #[test]
    fn unknown_cross_model_measurements_fall_back_to_normal_prefill() {
        use crate::kv::{
            KvCompatibilityRecord, KvStrategy, ModelCapabilities, ModelMetadata, ModelReference,
        };
        let mut kv = KvIntelligence::default();
        for id in ["model-a", "model-b"] {
            kv.models
                .register(ModelMetadata {
                    model: ModelReference::new(id, "1"),
                    kv_layout: "layout-v1".into(),
                    capabilities: ModelCapabilities::normal_prefill()
                        .with_kv(KvStrategy::CrossModelKvTransfer),
                })
                .unwrap();
        }
        kv.compatibility
            .register(KvCompatibilityRecord::unknown(
                ModelReference::new("model-a", "1"),
                ModelReference::new("model-b", "1"),
            ))
            .unwrap();
        let plan = Planner::new(sample_router())
            .with_kv_intelligence(kv)
            .plan_with_kv(
                PlanningRequest::new("acme", "start", "done"),
                KvPlanningContext {
                    target: ModelReference::new("model-b", "1"),
                    source: Some(ModelReference::new("model-a", "1")),
                    requested: KvStrategy::CrossModelKvTransfer,
                },
            )
            .unwrap();
        let PlanOutcome::Execute(plan) = plan else {
            panic!("expected plan")
        };
        assert_eq!(plan.kv.strategy, KvStrategy::None);
        assert!(plan.kv.fallback_reason.is_some());
    }
    fn sample_router() -> Router {
        let router = Router::new();
        router.add_tenant("acme");
        router.add_edge("acme", Edge::new("start", "primary"));
        router.add_edge("acme", Edge::new("primary", "done"));
        router.add_edge(
            "acme",
            Edge::new("start", "fallback").with_costs(10.0, 0.0, 10, 0.0),
        );
        router.add_edge(
            "acme",
            Edge::new("fallback", "done").with_costs(10.0, 0.0, 10, 0.0),
        );
        router
    }
}
