pub mod admission;
pub mod backend;
pub mod backend_registry;
pub mod capacity;
pub mod control_plane;
pub mod decision_trace;
pub mod escalation;
pub mod evaluation;
pub mod execution;
pub mod health_monitor;
pub mod inference;
pub mod kv;
pub mod planner;
pub mod sidecar_config;
pub mod sidecar_security;
pub mod system_planning;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde_json::json;

pub type TenantId = String;
pub type NodeId = String;

pub const OPEN_THRESHOLD: u32 = 5;
pub const DEGRADED_THRESHOLD: u32 = 2;
pub const HALF_OPEN_SUCCESS_NEEDED: u32 = 3;
pub const DEGRADED_SUCCESS_NEEDED: u32 = 2;
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);
pub const LATENCY_SAMPLE_LIMIT: usize = 128;
pub const ROUTE_CACHE_LIMIT: usize = 1024;
pub const AUDIT_EVENT_MEMORY_LIMIT: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthPolicy {
    pub open_threshold: u32,
    pub degraded_threshold: u32,
    pub half_open_success_needed: u32,
    pub degraded_success_needed: u32,
    pub cooldown: Duration,
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            open_threshold: OPEN_THRESHOLD,
            degraded_threshold: DEGRADED_THRESHOLD,
            half_open_success_needed: HALF_OPEN_SUCCESS_NEEDED,
            degraded_success_needed: DEGRADED_SUCCESS_NEEDED,
            cooldown: DEFAULT_COOLDOWN,
        }
    }
}

impl HealthPolicy {
    pub fn validate(self) -> Result<(), RouterError> {
        if self.degraded_threshold == 0 || self.open_threshold == 0 {
            return Err(RouterError::InvalidHealthPolicy(
                "failure thresholds must be greater than zero",
            ));
        }
        if self.degraded_threshold > self.open_threshold {
            return Err(RouterError::InvalidHealthPolicy(
                "degraded_threshold cannot exceed open_threshold",
            ));
        }
        if self.half_open_success_needed == 0 || self.degraded_success_needed == 0 {
            return Err(RouterError::InvalidHealthPolicy(
                "success thresholds must be greater than zero",
            ));
        }
        if self.cooldown.is_zero() {
            return Err(RouterError::InvalidHealthPolicy(
                "cooldown must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterError {
    InvalidCostModel(&'static str),
    InvalidHealthPolicy(&'static str),
}

impl std::fmt::Display for RouterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCostModel(message) => write!(formatter, "invalid cost model: {message}"),
            Self::InvalidHealthPolicy(message) => {
                write!(formatter, "invalid health policy: {message}")
            }
        }
    }
}

impl std::error::Error for RouterError {}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostWeights {
    pub w_dollar: f64,
    pub w_latency: f64,
    pub w_risk: f64,
    pub w_base: f64,
}

impl Default for CostWeights {
    fn default() -> Self {
        Self {
            w_dollar: 0.4,
            w_latency: 0.3,
            w_risk: 0.2,
            w_base: 0.1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostCeilings {
    pub max_dollar: f64,
    pub max_latency_ms: u64,
    pub max_base: f64,
}

impl Default for CostCeilings {
    fn default() -> Self {
        Self {
            max_dollar: 0.01,
            max_latency_ms: 1_000,
            max_base: 10.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TenantCostModel {
    pub weights: CostWeights,
    pub ceilings: CostCeilings,
}

impl TenantCostModel {
    pub fn validate(self) -> Result<(), RouterError> {
        let weights = [
            self.weights.w_dollar,
            self.weights.w_latency,
            self.weights.w_risk,
            self.weights.w_base,
        ];
        if weights
            .iter()
            .any(|weight| !weight.is_finite() || *weight < 0.0)
        {
            return Err(RouterError::InvalidCostModel(
                "weights must be finite and non-negative",
            ));
        }
        if weights.iter().all(|weight| *weight == 0.0) {
            return Err(RouterError::InvalidCostModel(
                "at least one weight must be greater than zero",
            ));
        }
        if !self.ceilings.max_dollar.is_finite()
            || !self.ceilings.max_base.is_finite()
            || self.ceilings.max_dollar <= 0.0
            || self.ceilings.max_base <= 0.0
            || self.ceilings.max_latency_ms == 0
        {
            return Err(RouterError::InvalidCostModel(
                "ceilings must be finite and greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub base_cost: f64,
    pub dollar_cost: f64,
    pub latency_ms: u64,
    pub risk: f64,
    pub enabled: bool,
}

impl Edge {
    pub fn new(from: impl Into<NodeId>, to: impl Into<NodeId>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            base_cost: 1.0,
            dollar_cost: 0.0,
            latency_ms: 0,
            risk: 0.0,
            enabled: true,
        }
    }

    pub fn with_costs(
        mut self,
        base_cost: f64,
        dollar_cost: f64,
        latency_ms: u64,
        risk: f64,
    ) -> Self {
        self.base_cost = base_cost;
        self.dollar_cost = dollar_cost;
        self.latency_ms = latency_ms;
        self.risk = risk;
        self
    }

    pub fn is_valid(&self) -> bool {
        !self.from.is_empty()
            && !self.to.is_empty()
            && self.base_cost.is_finite()
            && self.dollar_cost.is_finite()
            && self.risk.is_finite()
            && self.base_cost >= 0.0
            && self.dollar_cost >= 0.0
            && self.risk >= 0.0
    }

    pub fn weight(&self, cost_model: TenantCostModel) -> f64 {
        if !self.is_valid() || cost_model.validate().is_err() {
            return f64::INFINITY;
        }
        let ceilings = cost_model.ceilings;
        let weights = cost_model.weights;
        let dollar = normalize(self.dollar_cost, ceilings.max_dollar);
        let latency = normalize(self.latency_ms as f64, ceilings.max_latency_ms as f64);
        let risk = self.risk.clamp(0.0, 1.0);
        let base = normalize(self.base_cost, ceilings.max_base);

        weights.w_dollar * dollar
            + weights.w_latency * latency
            + weights.w_risk * risk
            + weights.w_base * base
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    Healthy,
    Degraded,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HealthTracker {
    pub state: NodeState,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub p99_latency_ms: u64,
    pub success_rate: f64,
    pub opened_at: Option<Instant>,
    latency_samples: VecDeque<u64>,
}

impl Default for HealthTracker {
    fn default() -> Self {
        Self {
            state: NodeState::Healthy,
            consecutive_failures: 0,
            consecutive_successes: 0,
            p99_latency_ms: 0,
            success_rate: 1.0,
            opened_at: None,
            latency_samples: VecDeque::new(),
        }
    }
}

impl HealthTracker {
    pub fn record_result(&mut self, success: bool, latency_ms: u64) -> bool {
        self.record_result_with_policy(success, latency_ms, HealthPolicy::default())
    }

    pub fn record_result_with_policy(
        &mut self,
        success: bool,
        latency_ms: u64,
        policy: HealthPolicy,
    ) -> bool {
        let previous_state = self.state;
        self.record_latency_sample(latency_ms);

        if success {
            self.consecutive_successes += 1;
            self.consecutive_failures = 0;
            self.success_rate = ((self.success_rate * 9.0) + 1.0) / 10.0;
            match self.state {
                NodeState::HalfOpen
                    if self.consecutive_successes >= policy.half_open_success_needed =>
                {
                    self.state = NodeState::Healthy;
                    self.opened_at = None;
                }
                NodeState::Degraded
                    if self.consecutive_successes >= policy.degraded_success_needed =>
                {
                    self.state = NodeState::Healthy;
                    self.opened_at = None;
                }
                _ => {}
            }
        } else {
            self.consecutive_failures += 1;
            self.consecutive_successes = 0;
            self.success_rate = (self.success_rate * 9.0) / 10.0;
            self.state = match self.consecutive_failures {
                failures if failures >= policy.open_threshold => NodeState::Open,
                failures if failures >= policy.degraded_threshold => NodeState::Degraded,
                _ => self.state,
            };
            if self.state == NodeState::Open && previous_state != NodeState::Open {
                self.opened_at = Some(Instant::now());
            }
        }

        previous_state != self.state
    }

    fn record_latency_sample(&mut self, latency_ms: u64) {
        if self.latency_samples.len() == LATENCY_SAMPLE_LIMIT {
            self.latency_samples.pop_front();
        }
        self.latency_samples.push_back(latency_ms);
        let mut samples: Vec<_> = self.latency_samples.iter().copied().collect();
        samples.sort_unstable();
        let index = ((samples.len().saturating_sub(1)) as f64 * 0.99).round() as usize;
        self.p99_latency_ms = samples.get(index).copied().unwrap_or(latency_ms);
    }

    pub fn transition_to_half_open_if_ready(&mut self, cooldown: Duration, now: Instant) -> bool {
        if self.state != NodeState::Open {
            return false;
        }

        let Some(opened_at) = self.opened_at else {
            return false;
        };

        if now.duration_since(opened_at) < cooldown {
            return false;
        }

        self.state = NodeState::HalfOpen;
        self.consecutive_successes = 0;
        true
    }

    pub fn force_half_open(&mut self) -> bool {
        if self.state != NodeState::Open {
            return false;
        }

        self.state = NodeState::HalfOpen;
        self.consecutive_successes = 0;
        true
    }

    pub fn penalty(&self) -> f64 {
        let latency_penalty = self.p99_latency_ms as f64 / 10_000.0;
        let success_penalty = 1.0 - self.success_rate.clamp(0.0, 1.0);

        match self.state {
            NodeState::Healthy => latency_penalty + success_penalty,
            NodeState::Degraded => 0.5 + latency_penalty + success_penalty,
            NodeState::HalfOpen => 0.3 + latency_penalty + success_penalty,
            NodeState::Open => f64::INFINITY,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub nodes: Vec<NodeId>,
    pub total_cost: f64,
    pub cache_hit: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RouteDecision {
    Routed(Route),
    Escalate(EscalationContext),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EscalationContext {
    pub tenant_id: TenantId,
    pub start: NodeId,
    pub goal: NodeId,
    pub reason: EscalationReason,
    pub failed_or_blocked_nodes: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EscalationReason {
    NoFeasiblePath,
    TenantNotFound,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuditEvent {
    Reroute {
        tenant_id: TenantId,
        start: NodeId,
        goal: NodeId,
        path: Vec<NodeId>,
        total_cost: f64,
        cache_hit: bool,
    },
    ExplicitEscalation(EscalationContext),
    HealthChanged {
        tenant_id: TenantId,
        node_id: NodeId,
        state: NodeState,
        state_changed: bool,
    },
    RecoveryProbeOpened {
        tenant_id: TenantId,
        node_id: NodeId,
    },
}

impl AuditEvent {
    pub fn to_json_line(&self) -> String {
        let value = match self {
            AuditEvent::Reroute {
                tenant_id,
                start,
                goal,
                path,
                total_cost,
                cache_hit,
            } => json!({
                "event": "reroute",
                "tenant_id": tenant_id,
                "start": start,
                "goal": goal,
                "path": path,
                "total_cost": total_cost,
                "cache_hit": cache_hit,
            }),
            AuditEvent::ExplicitEscalation(context) => json!({
                "event": "explicit_escalation",
                "tenant_id": &context.tenant_id,
                "start": &context.start,
                "goal": &context.goal,
                "reason": format!("{:?}", context.reason),
                "failed_or_blocked_nodes": &context.failed_or_blocked_nodes,
            }),
            AuditEvent::HealthChanged {
                tenant_id,
                node_id,
                state,
                state_changed,
            } => json!({
                "event": "health_changed",
                "tenant_id": tenant_id,
                "node_id": node_id,
                "state": format!("{:?}", state),
                "state_changed": state_changed,
            }),
            AuditEvent::RecoveryProbeOpened { tenant_id, node_id } => json!({
                "event": "recovery_probe_opened",
                "tenant_id": tenant_id,
                "node_id": node_id,
            }),
        };

        serde_json::to_string(&value).expect("audit events serialize to JSON")
    }
}

pub fn append_events_jsonl(path: impl AsRef<Path>, events: &[AuditEvent]) -> io::Result<usize> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;

    for event in events {
        writeln!(file, "{}", event.to_json_line())?;
    }

    file.sync_all()?;
    Ok(events.len())
}

#[derive(Debug, Default, Clone)]
pub struct Router {
    inner: Arc<RwLock<RouterState>>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_tenant(&self, tenant_id: impl Into<TenantId>) {
        let mut state = self.write_state();
        state.tenants.entry(tenant_id.into()).or_default();
    }

    pub fn set_cost_model(&self, tenant_id: impl Into<TenantId>, cost_model: TenantCostModel) {
        let _ = self.try_set_cost_model(tenant_id, cost_model);
    }

    pub fn try_set_cost_model(
        &self,
        tenant_id: impl Into<TenantId>,
        cost_model: TenantCostModel,
    ) -> Result<(), RouterError> {
        cost_model.validate()?;
        let mut state = self.write_state();
        let tenant = state.tenants.entry(tenant_id.into()).or_default();
        tenant.cost_model = cost_model;
        tenant.invalidate_all_cache();
        Ok(())
    }

    pub fn try_set_health_policy(
        &self,
        tenant_id: impl Into<TenantId>,
        policy: HealthPolicy,
    ) -> Result<(), RouterError> {
        policy.validate()?;
        let mut state = self.write_state();
        let tenant = state.tenants.entry(tenant_id.into()).or_default();
        tenant.health_policy = policy;
        tenant.invalidate_all_cache();
        Ok(())
    }

    pub fn add_edge(&self, tenant_id: impl Into<TenantId>, edge: Edge) {
        let mut state = self.write_state();
        let tenant = state.tenants.entry(tenant_id.into()).or_default();
        tenant.graph.add_edge(edge);
        tenant.invalidate_all_cache();
    }

    pub fn record_result(
        &self,
        tenant_id: impl Into<TenantId>,
        node_id: impl Into<NodeId>,
        success: bool,
        latency_ms: u64,
    ) {
        let tenant_id = tenant_id.into();
        let node_id = node_id.into();
        let mut state = self.write_state();
        let tenant = state.tenants.entry(tenant_id.clone()).or_default();
        let tracker = tenant.health.entry(node_id.clone()).or_default();
        let state_changed =
            tracker.record_result_with_policy(success, latency_ms, tenant.health_policy);
        let node_state = tracker.state;

        if state_changed {
            tenant.invalidate_for_node(&node_id);
        }

        state.push_event(AuditEvent::HealthChanged {
            tenant_id,
            node_id,
            state: node_state,
            state_changed,
        });
    }

    pub fn force_half_open(
        &self,
        tenant_id: impl Into<TenantId>,
        node_id: impl Into<NodeId>,
    ) -> bool {
        let tenant_id = tenant_id.into();
        let node_id = node_id.into();
        let mut state = self.write_state();
        let Some(tenant) = state.tenants.get_mut(&tenant_id) else {
            return false;
        };
        let Some(tracker) = tenant.health.get_mut(&node_id) else {
            return false;
        };

        if !tracker.force_half_open() {
            return false;
        }

        tenant.invalidate_for_node(&node_id);
        state.push_event(AuditEvent::RecoveryProbeOpened { tenant_id, node_id });
        true
    }

    pub fn spawn_recovery_loop(
        &self,
        cooldown: Duration,
        scan_interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let router = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(scan_interval);
            loop {
                ticker.tick().await;
                router.run_recovery_probe_once(cooldown);
            }
        })
    }

    pub fn spawn_default_recovery_loop(&self) -> tokio::task::JoinHandle<()> {
        self.spawn_recovery_loop(DEFAULT_COOLDOWN, DEFAULT_COOLDOWN)
    }

    pub fn run_recovery_probe_once(&self, cooldown: Duration) -> usize {
        let now = Instant::now();
        let mut opened = Vec::new();
        let mut state = self.write_state();

        for (tenant_id, tenant) in &mut state.tenants {
            for (node_id, tracker) in &mut tenant.health {
                if tracker.transition_to_half_open_if_ready(cooldown, now) {
                    opened.push((tenant_id.clone(), node_id.clone()));
                }
            }
        }

        for (tenant_id, node_id) in &opened {
            if let Some(tenant) = state.tenants.get_mut(tenant_id) {
                tenant.invalidate_for_node(node_id);
            }
            state.push_event(AuditEvent::RecoveryProbeOpened {
                tenant_id: tenant_id.clone(),
                node_id: node_id.clone(),
            });
        }

        opened.len()
    }

    pub fn route(
        &self,
        tenant_id: impl Into<TenantId>,
        start: impl Into<NodeId>,
        goal: impl Into<NodeId>,
    ) -> RouteDecision {
        let tenant_id = tenant_id.into();
        let start = start.into();
        let goal = goal.into();
        let mut state = self.write_state();

        let Some(tenant) = state.tenants.get_mut(&tenant_id) else {
            return state.escalate(EscalationContext {
                tenant_id,
                start,
                goal,
                reason: EscalationReason::TenantNotFound,
                failed_or_blocked_nodes: Vec::new(),
            });
        };

        let cache_key = RouteKey {
            start: start.clone(),
            goal: goal.clone(),
        };

        if let Some(cached) = tenant.get_cached(&cache_key) {
            let route = Route {
                nodes: cached.path,
                total_cost: cached.weight,
                cache_hit: true,
            };
            state.push_event(AuditEvent::Reroute {
                tenant_id,
                start,
                goal,
                path: route.nodes.clone(),
                total_cost: route.total_cost,
                cache_hit: true,
            });
            return RouteDecision::Routed(route);
        }

        match tenant
            .graph
            .shortest_path(&tenant.health, tenant.cost_model, &start, &goal)
        {
            Some(route) => {
                tenant.insert_cache(
                    cache_key,
                    CachedPath {
                        path: route.nodes.clone(),
                        weight: route.total_cost,
                        touched_nodes: route.nodes.iter().cloned().collect(),
                    },
                );
                state.push_event(AuditEvent::Reroute {
                    tenant_id,
                    start,
                    goal,
                    path: route.nodes.clone(),
                    total_cost: route.total_cost,
                    cache_hit: false,
                });
                RouteDecision::Routed(route)
            }
            None => {
                let failed_or_blocked_nodes = tenant.blocked_nodes();
                state.escalate(EscalationContext {
                    tenant_id,
                    start,
                    goal,
                    reason: EscalationReason::NoFeasiblePath,
                    failed_or_blocked_nodes,
                })
            }
        }
    }

    pub fn events(&self) -> Vec<AuditEvent> {
        self.read_state().events.iter().cloned().collect()
    }

    pub fn last_audit_event(&self) -> Option<AuditEvent> {
        self.read_state().events.back().cloned()
    }

    pub fn node_state(
        &self,
        tenant_id: impl AsRef<str>,
        node_id: impl AsRef<str>,
    ) -> Option<NodeState> {
        self.read_state()
            .tenants
            .get(tenant_id.as_ref())?
            .health
            .get(node_id.as_ref())
            .map(|tracker| tracker.state)
    }

    pub fn append_audit_jsonl(&self, path: impl AsRef<Path>) -> io::Result<usize> {
        let events = self.events();
        append_events_jsonl(path, &events)
    }

    fn read_state(&self) -> std::sync::RwLockReadGuard<'_, RouterState> {
        self.inner.read().expect("router lock poisoned")
    }

    fn write_state(&self) -> std::sync::RwLockWriteGuard<'_, RouterState> {
        self.inner.write().expect("router lock poisoned")
    }
}

#[derive(Debug, Default)]
struct RouterState {
    tenants: HashMap<TenantId, TenantState>,
    events: VecDeque<AuditEvent>,
}

impl RouterState {
    fn push_event(&mut self, event: AuditEvent) {
        if self.events.len() == AUDIT_EVENT_MEMORY_LIMIT {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn escalate(&mut self, context: EscalationContext) -> RouteDecision {
        self.push_event(AuditEvent::ExplicitEscalation(context.clone()));
        RouteDecision::Escalate(context)
    }
}

#[derive(Debug, Default)]
struct TenantState {
    graph: ToolGraph,
    health: HashMap<NodeId, HealthTracker>,
    cache: HashMap<RouteKey, CachedPath>,
    cache_order: VecDeque<RouteKey>,
    cost_model: TenantCostModel,
    health_policy: HealthPolicy,
}

impl TenantState {
    fn invalidate_all_cache(&mut self) {
        self.cache.clear();
        self.cache_order.clear();
    }

    fn invalidate_for_node(&mut self, node: &str) {
        self.cache
            .retain(|_, cached| !cached.touched_nodes.contains(node));
        self.cache_order.retain(|key| self.cache.contains_key(key));
    }

    fn get_cached(&mut self, key: &RouteKey) -> Option<CachedPath> {
        let cached = self.cache.get(key)?.clone();
        self.cache_order.retain(|existing| existing != key);
        self.cache_order.push_back(key.clone());
        Some(cached)
    }

    fn insert_cache(&mut self, key: RouteKey, cached: CachedPath) {
        if !self.cache.contains_key(&key)
            && self.cache.len() == ROUTE_CACHE_LIMIT
            && let Some(evicted) = self.cache_order.pop_front()
        {
            self.cache.remove(&evicted);
        }
        self.cache_order.retain(|existing| existing != &key);
        self.cache_order.push_back(key.clone());
        self.cache.insert(key, cached);
    }

    fn blocked_nodes(&self) -> Vec<NodeId> {
        let mut nodes: Vec<_> = self
            .health
            .iter()
            .filter_map(|(node, health)| (health.state == NodeState::Open).then_some(node.clone()))
            .collect();
        nodes.sort();
        nodes
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CachedPath {
    pub path: Vec<NodeId>,
    pub weight: f64,
    pub touched_nodes: HashSet<NodeId>,
}

#[derive(Debug, Default)]
struct ToolGraph {
    adjacency: HashMap<NodeId, Vec<Edge>>,
}

impl ToolGraph {
    fn add_edge(&mut self, edge: Edge) {
        self.adjacency
            .entry(edge.from.clone())
            .or_default()
            .push(edge);
    }

    fn shortest_path(
        &self,
        health: &HashMap<NodeId, HealthTracker>,
        cost_model: TenantCostModel,
        start: &str,
        goal: &str,
    ) -> Option<Route> {
        let mut distances: HashMap<NodeId, f64> = HashMap::new();
        let mut previous: HashMap<NodeId, NodeId> = HashMap::new();
        let mut heap = BinaryHeap::new();

        distances.insert(start.to_string(), 0.0);
        heap.push(SearchState {
            node: start.to_string(),
            cost: 0.0,
        });

        while let Some(SearchState { node, cost }) = heap.pop() {
            if node == goal {
                return Some(Route {
                    nodes: reconstruct_path(previous, start, goal),
                    total_cost: cost,
                    cache_hit: false,
                });
            }

            if cost > *distances.get(&node).unwrap_or(&f64::INFINITY) {
                continue;
            }

            let Some(edges) = self.adjacency.get(&node) else {
                continue;
            };

            for edge in edges {
                if !edge.enabled {
                    continue;
                }

                let health_penalty = health
                    .get(&edge.to)
                    .map(HealthTracker::penalty)
                    .unwrap_or(0.0);
                if !edge.is_valid() {
                    continue;
                }

                let edge_cost = edge.weight(cost_model) + health_penalty;
                if edge_cost.is_infinite() || edge_cost.is_nan() {
                    continue;
                }

                let next_cost = cost + edge_cost;
                let best_known = distances.get(&edge.to).copied().unwrap_or(f64::INFINITY);
                if next_cost < best_known {
                    distances.insert(edge.to.clone(), next_cost);
                    previous.insert(edge.to.clone(), node.clone());
                    heap.push(SearchState {
                        node: edge.to.clone(),
                        cost: next_cost,
                    });
                }
            }
        }

        None
    }
}

fn normalize(value: f64, ceiling: f64) -> f64 {
    if ceiling <= 0.0 {
        return 1.0;
    }

    (value / ceiling).clamp(0.0, 1.0)
}

#[allow(dead_code)]
fn json_string_array(values: &[String]) -> String {
    let items: Vec<_> = values
        .iter()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .collect();
    format!("[{}]", items.join(","))
}

#[allow(dead_code)]
fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            _ => vec![ch],
        })
        .collect()
}

fn reconstruct_path(previous: HashMap<NodeId, NodeId>, start: &str, goal: &str) -> Vec<NodeId> {
    let mut path = vec![goal.to_string()];
    let mut cursor = goal;

    while cursor != start {
        let Some(prev) = previous.get(cursor) else {
            break;
        };
        path.push(prev.clone());
        cursor = prev;
    }

    path.reverse();
    path
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RouteKey {
    start: NodeId,
    goal: NodeId,
}

#[derive(Debug, Clone, PartialEq)]
struct SearchState {
    node: NodeId,
    cost: f64,
}

impl Eq for SearchState {}

impl Ord for SearchState {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.node.cmp(&other.node))
    }
}

impl PartialOrd for SearchState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn normalizes_edge_cost_with_tenant_weights_and_ceilings() {
        let edge = Edge::new("a", "b").with_costs(5.0, 0.005, 500, 0.5);
        let model = TenantCostModel::default();

        assert!((edge.weight(model) - 0.5).abs() < 0.0001);
    }

    #[test]
    fn rejects_invalid_cost_models_without_replacing_the_current_model() {
        let router = sample_router();
        let invalid = TenantCostModel {
            weights: CostWeights {
                w_dollar: f64::NAN,
                ..CostWeights::default()
            },
            ..TenantCostModel::default()
        };

        assert!(router.try_set_cost_model("acme", invalid).is_err());
        assert!(matches!(
            router.route("acme", "start", "done"),
            RouteDecision::Routed(_)
        ));
    }

    #[test]
    fn applies_a_validated_tenant_health_policy() {
        let router = sample_router();
        router
            .try_set_health_policy(
                "acme",
                HealthPolicy {
                    open_threshold: 2,
                    degraded_threshold: 1,
                    half_open_success_needed: 2,
                    degraded_success_needed: 1,
                    cooldown: Duration::from_millis(10),
                },
            )
            .expect("policy is valid");

        router.record_result("acme", "search", false, 100);
        assert_eq!(
            router.node_state("acme", "search"),
            Some(NodeState::Degraded)
        );
        router.record_result("acme", "search", false, 100);
        assert_eq!(router.node_state("acme", "search"), Some(NodeState::Open));
    }

    #[test]
    fn routes_through_lowest_cost_path() {
        let router = sample_router();

        let RouteDecision::Routed(route) = router.route("acme", "start", "done") else {
            panic!("expected route");
        };

        assert_eq!(route.nodes, vec!["start", "search", "summarize", "done"]);
        assert!(!route.cache_hit);
    }

    #[test]
    fn degraded_tool_is_penalized_but_still_routable() {
        let router = sample_router();
        router.record_result("acme", "search", false, 1_000);
        router.record_result("acme", "search", false, 1_000);

        let RouteDecision::Routed(route) = router.route("acme", "start", "done") else {
            panic!("expected route");
        };

        assert_eq!(route.nodes, vec!["start", "search", "summarize", "done"]);
    }

    #[test]
    fn reroutes_around_open_tool() {
        let router = sample_router();
        open_node(&router, "search");

        let RouteDecision::Routed(route) = router.route("acme", "start", "done") else {
            panic!("expected route");
        };

        assert_eq!(
            route.nodes,
            vec!["start", "fallback_search", "summarize", "done"]
        );
    }

    #[test]
    fn escalates_when_no_feasible_path_exists() {
        let router = sample_router();
        open_node(&router, "search");
        open_node(&router, "fallback_search");

        let RouteDecision::Escalate(context) = router.route("acme", "start", "done") else {
            panic!("expected escalation");
        };

        assert_eq!(context.reason, EscalationReason::NoFeasiblePath);
        assert_eq!(
            context.failed_or_blocked_nodes,
            vec!["fallback_search", "search"]
        );
    }

    #[test]
    fn invalidates_only_cache_entries_that_touch_changed_node() {
        let router = sample_router();
        let _ = router.route("acme", "start", "done");
        let _ = router.route("acme", "start", "audit_done");

        open_node(&router, "search");

        let RouteDecision::Routed(done_route) = router.route("acme", "start", "done") else {
            panic!("expected route");
        };
        let RouteDecision::Routed(audit_route) = router.route("acme", "start", "audit_done") else {
            panic!("expected route");
        };

        assert!(!done_route.cache_hit);
        assert!(audit_route.cache_hit);
    }

    #[test]
    fn cache_survives_health_ping_without_state_change() {
        let router = sample_router();

        assert_routed_cache_state(&router, false);
        assert_routed_cache_state(&router, true);
        router.record_result("acme", "search", true, 100);
        assert_routed_cache_state(&router, true);
    }

    #[test]
    fn half_open_recovery_probe_reintroduces_open_node_with_penalty() {
        let router = sample_router();
        open_node(&router, "search");

        assert!(router.force_half_open("acme", "search"));

        let RouteDecision::Routed(route) = router.route("acme", "start", "done") else {
            panic!("expected route");
        };

        assert_eq!(route.nodes, vec!["start", "search", "summarize", "done"]);
    }

    #[test]
    fn keeps_tenant_health_isolated() {
        let router = sample_router();
        router.add_edge(
            "globex",
            Edge::new("start", "search").with_costs(1.0, 0.001, 100, 0.1),
        );
        router.add_edge(
            "globex",
            Edge::new("search", "done").with_costs(1.0, 0.001, 100, 0.1),
        );

        open_node(&router, "search");

        let RouteDecision::Routed(route) = router.route("globex", "start", "done") else {
            panic!("expected route");
        };

        assert_eq!(route.nodes, vec!["start", "search", "done"]);
    }

    #[test]
    fn writes_audit_events_for_routing_and_escalation() {
        let router = sample_router();
        let _ = router.route("acme", "start", "done");
        open_node(&router, "search");
        open_node(&router, "fallback_search");
        let _ = router.route("acme", "start", "done");

        let events = router.events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AuditEvent::Reroute { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AuditEvent::ExplicitEscalation(_)))
        );
    }

    #[test]
    fn generated_graph_failures_are_logged_as_reroute_or_escalation() {
        for node_count in 3..12 {
            for failed_index in 1..(node_count - 1) {
                let router = generated_router(node_count);
                for _ in 0..OPEN_THRESHOLD {
                    router.record_result("generated", format!("node_{failed_index}"), false, 1_000);
                }

                let _ = router.route("generated", "node_0", format!("node_{}", node_count - 1));
                assert!(matches!(
                    router.last_audit_event(),
                    Some(AuditEvent::Reroute { .. } | AuditEvent::ExplicitEscalation(_))
                ));
            }
        }
    }

    #[test]
    fn chaos_trials_never_fail_silently() {
        let mut rng = DeterministicRng::new(0x5eed_1234);

        for trial in 0..200 {
            let node_count = 5 + rng.next_usize(11);
            let router = chaos_router(trial, node_count, &mut rng);
            let failed_count = 1 + rng.next_usize(3);
            let mut failed_nodes = HashSet::new();

            while failed_nodes.len() < failed_count {
                let index = 1 + rng.next_usize(node_count - 2);
                failed_nodes.insert(format!("node_{trial}_{index}"));
            }

            for node in &failed_nodes {
                for _ in 0..OPEN_THRESHOLD {
                    router.record_result(format!("chaos_{trial}"), node, false, 250);
                }
            }

            let _ = router.route(
                format!("chaos_{trial}"),
                format!("node_{trial}_0"),
                format!("node_{trial}_{}", node_count - 1),
            );

            assert!(
                matches!(
                    router.last_audit_event(),
                    Some(AuditEvent::Reroute { .. } | AuditEvent::ExplicitEscalation(_))
                ),
                "trial {trial} failed silently after opening nodes {:?}",
                failed_nodes
            );
        }
    }

    #[test]
    fn concurrent_route_and_health_updates_do_not_panic() {
        let router = sample_router();
        let mut handles = Vec::new();

        for worker in 0..8 {
            let router = router.clone();
            handles.push(thread::spawn(move || {
                for iteration in 0..100 {
                    if worker % 2 == 0 {
                        let _ = router.route("acme", "start", "done");
                    } else {
                        router.record_result(
                            "acme",
                            if iteration % 2 == 0 {
                                "search"
                            } else {
                                "fallback_search"
                            },
                            iteration % 5 != 0,
                            100,
                        );
                    }
                }
            }));
        }

        for handle in handles {
            handle.join().expect("worker panicked");
        }
    }

    #[test]
    fn exports_audit_events_as_jsonl() {
        let router = sample_router();
        let _ = router.route("acme", "start", "done");
        let path =
            std::env::temp_dir().join(format!("trust-router-audit-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let written = router
            .append_audit_jsonl(&path)
            .expect("audit export failed");
        let contents = std::fs::read_to_string(&path).expect("audit file missing");

        assert_eq!(written, 1);
        assert!(contents.contains("\"event\":\"reroute\""));

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn automatic_recovery_loop_transitions_open_node_to_half_open() {
        let router = sample_router();
        open_node(&router, "search");
        assert_eq!(router.node_state("acme", "search"), Some(NodeState::Open));

        let handle =
            router.spawn_recovery_loop(Duration::from_millis(10), Duration::from_millis(5));
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        while router.node_state("acme", "search") != Some(NodeState::HalfOpen)
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        handle.abort();

        assert_eq!(
            router.node_state("acme", "search"),
            Some(NodeState::HalfOpen)
        );
        assert!(
            router
                .events()
                .iter()
                .any(|event| matches!(event, AuditEvent::RecoveryProbeOpened { .. }))
        );
    }

    #[test]
    fn latency_p99_uses_a_bounded_rolling_sample() {
        let mut tracker = HealthTracker::default();

        for latency_ms in 1..=200 {
            tracker.record_result(true, latency_ms);
        }
        let p99_before_latest_sample = tracker.p99_latency_ms;
        tracker.record_result(true, 1);

        assert_eq!(tracker.latency_samples.len(), LATENCY_SAMPLE_LIMIT);
        assert!(p99_before_latest_sample >= 198);
        assert!(tracker.p99_latency_ms >= 198);
    }

    #[test]
    fn half_open_failure_reopens_the_circuit() {
        let router = sample_router();
        open_node(&router, "search");
        assert!(router.force_half_open("acme", "search"));

        router.record_result("acme", "search", false, 1_000);

        assert_eq!(router.node_state("acme", "search"), Some(NodeState::Open));
    }

    #[test]
    fn start_equal_to_goal_is_a_zero_cost_route() {
        let router = sample_router();

        let RouteDecision::Routed(route) = router.route("acme", "start", "start") else {
            panic!("expected a route");
        };

        assert_eq!(route.nodes, vec!["start"]);
        assert_eq!(route.total_cost, 0.0);
        assert!(!route.cache_hit);
    }

    #[test]
    fn route_cache_has_a_fixed_capacity() {
        let router = Router::new();
        router.add_tenant("bounded-cache");

        for index in 0..(ROUTE_CACHE_LIMIT + 8) {
            router.add_edge(
                "bounded-cache",
                Edge::new(format!("start_{index}"), format!("goal_{index}"))
                    .with_costs(1.0, 0.001, 10, 0.1),
            );
        }

        for index in 0..(ROUTE_CACHE_LIMIT + 8) {
            assert!(matches!(
                router.route(
                    "bounded-cache",
                    format!("start_{index}"),
                    format!("goal_{index}")
                ),
                RouteDecision::Routed(_)
            ));
        }

        let state = router.read_state();
        let tenant = state.tenants.get("bounded-cache").expect("tenant exists");
        assert_eq!(tenant.cache.len(), ROUTE_CACHE_LIMIT);
        assert_eq!(tenant.cache_order.len(), ROUTE_CACHE_LIMIT);
    }

    #[test]
    fn audit_event_history_has_a_fixed_capacity() {
        let router = sample_router();

        for _ in 0..(AUDIT_EVENT_MEMORY_LIMIT + 8) {
            let _ = router.route("acme", "start", "done");
        }

        assert_eq!(router.events().len(), AUDIT_EVENT_MEMORY_LIMIT);
    }

    fn assert_routed_cache_state(router: &Router, expected_cache_hit: bool) {
        let RouteDecision::Routed(route) = router.route("acme", "start", "done") else {
            panic!("expected route");
        };
        assert_eq!(route.cache_hit, expected_cache_hit);
    }

    fn open_node(router: &Router, node_id: &str) {
        for _ in 0..OPEN_THRESHOLD {
            router.record_result("acme", node_id, false, 1_000);
        }
    }

    fn sample_router() -> Router {
        let router = Router::new();
        router.add_tenant("acme");
        router.add_edge(
            "acme",
            Edge::new("start", "search").with_costs(1.0, 0.001, 100, 0.1),
        );
        router.add_edge(
            "acme",
            Edge::new("search", "summarize").with_costs(1.0, 0.002, 400, 0.2),
        );
        router.add_edge(
            "acme",
            Edge::new("start", "fallback_search").with_costs(10.0, 0.01, 1_000, 1.0),
        );
        router.add_edge(
            "acme",
            Edge::new("fallback_search", "summarize").with_costs(5.0, 0.005, 800, 0.5),
        );
        router.add_edge(
            "acme",
            Edge::new("summarize", "done").with_costs(1.0, 0.001, 100, 0.1),
        );
        router.add_edge(
            "acme",
            Edge::new("start", "audit").with_costs(1.0, 0.001, 100, 0.1),
        );
        router.add_edge(
            "acme",
            Edge::new("audit", "audit_done").with_costs(1.0, 0.001, 100, 0.1),
        );
        router
    }

    fn generated_router(node_count: usize) -> Router {
        let router = Router::new();
        router.add_tenant("generated");

        for index in 0..(node_count - 1) {
            router.add_edge(
                "generated",
                Edge::new(format!("node_{index}"), format!("node_{}", index + 1))
                    .with_costs(1.0, 0.001, 100, 0.1),
            );
        }

        router.add_edge(
            "generated",
            Edge::new("node_0", format!("node_{}", node_count - 1))
                .with_costs(5.0, 0.005, 900, 0.5),
        );
        router
    }

    fn chaos_router(trial: usize, node_count: usize, rng: &mut DeterministicRng) -> Router {
        let tenant = format!("chaos_{trial}");
        let router = Router::new();
        router.add_tenant(&tenant);

        for index in 0..(node_count - 1) {
            router.add_edge(
                &tenant,
                Edge::new(
                    format!("node_{trial}_{index}"),
                    format!("node_{trial}_{}", index + 1),
                )
                .with_costs(
                    1.0 + rng.next_usize(5) as f64,
                    0.001 * (1 + rng.next_usize(5)) as f64,
                    100 + rng.next_usize(400) as u64,
                    rng.next_usize(10) as f64 / 10.0,
                ),
            );
        }

        let extra_edges = node_count + rng.next_usize(node_count);
        for _ in 0..extra_edges {
            let from = rng.next_usize(node_count - 1);
            let to = from + 1 + rng.next_usize(node_count - from - 1);
            router.add_edge(
                &tenant,
                Edge::new(format!("node_{trial}_{from}"), format!("node_{trial}_{to}")).with_costs(
                    1.0 + rng.next_usize(8) as f64,
                    0.001 * (1 + rng.next_usize(8)) as f64,
                    100 + rng.next_usize(900) as u64,
                    rng.next_usize(10) as f64 / 10.0,
                ),
            );
        }

        router
    }

    struct DeterministicRng {
        state: u64,
    }

    impl DeterministicRng {
        fn new(seed: u64) -> Self {
            Self { state: seed }
        }

        fn next_usize(&mut self, upper_exclusive: usize) -> usize {
            assert!(upper_exclusive > 0);
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ((self.state >> 32) as usize) % upper_exclusive
        }
    }
}
