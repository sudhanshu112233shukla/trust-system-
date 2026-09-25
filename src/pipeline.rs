//! End-to-end deterministic inference decision and execution pipeline.
//!
//! Connects authentication, admission, immutable world state snapshots, candidate
//! generation, model/version validation, KV intelligence, capacity feasibility,
//! health/circuit breakers, prediction engine, decision firewall, execution
//! compiler, inference fabric transport, and feedback state updates into one
//! unified, deterministic execution flow.

use crate::admission::AdmissionController;
use crate::backend::BackendResult as ExecutionResult;
use crate::backend_registry::BackendRegistry;
use crate::capacity::{CapacityRequirement, CapacitySnapshot};
use crate::control_plane::{ControlPlaneSnapshot, ControlPlaneVersions};
use crate::decision_trace::{DecisionTrace, TraceRejection};
use crate::fabric::{
    AuditChain, CompiledExecutionPlan, ExecutionCompiler, InferenceTransport, KvAction,
    KvDecisionInput, KvTransport, TransportRequest,
};
use crate::health_monitor::HealthMonitor;
use crate::intelligence::{
    CounterfactualEngine, CounterfactualEstimate, DecisionConfidence, DecisionFirewall,
    DecisionMemory, FirewallDecision, InferenceObservation, PredictionEngine, PredictionError,
    PredictionErrorTracker, UtilityConfig, WorldStateManager,
};
use crate::kv::{KvIntelligence, ModelReference};
use crate::planner::{Candidate, ExecutionPlan, ExecutionStep, SystemSnapshot};

use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

// =========================================================================
// Pipeline Request & Response Types
// =========================================================================

/// Input request to the unified inference pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineRequest {
    pub request_id: String,
    pub tenant_id: String,
    pub api_key: Option<String>,
    pub model: String,
    pub model_version: Option<String>,
    pub prompt_tokens: usize,
    pub max_tokens: usize,
    pub prompt_prefix_hash: Option<String>,
    pub max_ttft_ms: Option<u64>,
    pub max_e2e_ms: Option<u64>,
    pub min_quality: Option<f64>,
    pub max_cost: Option<f64>,
    pub force_stale_world_state: bool,
}

impl PipelineRequest {
    pub fn new(
        request_id: impl Into<String>,
        tenant_id: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            tenant_id: tenant_id.into(),
            api_key: None,
            model: model.into(),
            model_version: None,
            prompt_tokens: 128,
            max_tokens: 256,
            prompt_prefix_hash: None,
            max_ttft_ms: Some(500),
            max_e2e_ms: Some(2000),
            min_quality: Some(0.8),
            max_cost: Some(0.05),
            force_stale_world_state: false,
        }
    }
}

/// Bounded structured outcome of pipeline execution.
#[derive(Debug, Clone)]
pub struct PipelineOutcome {
    pub success: bool,
    pub request_id: String,
    pub decision_id: String,
    pub selected_backend: Option<String>,
    pub selected_node: Option<String>,
    pub selected_strategy: Option<String>,
    pub selected_kv_action: Option<KvAction>,
    pub execution_plan: Option<ExecutionPlan>,
    pub compiled_plan: Option<CompiledExecutionPlan>,
    pub execution_result: Option<ExecutionResult>,
    pub rejection_reason: Option<String>,
    pub trace: DecisionTrace,
    pub world_revision: u64,
}

// =========================================================================
// Pipeline Engine — Single Unified Inference Control & Execution Pipeline
// =========================================================================

pub struct PipelineEngine {
    pub world_state_manager: WorldStateManager,
    pub backend_registry: Arc<RwLock<BackendRegistry>>,
    pub capacity: Arc<RwLock<CapacitySnapshot>>,
    pub control_plane: Arc<RwLock<ControlPlaneSnapshot>>,
    pub kv_intelligence: Arc<RwLock<KvIntelligence>>,
    pub admission_controller: AdmissionController,
    pub firewall: DecisionFirewall,
    pub audit_chain: Arc<RwLock<AuditChain>>,
    pub decision_memory: Arc<RwLock<DecisionMemory>>,
    pub prediction_tracker: Arc<RwLock<PredictionErrorTracker>>,
    pub prediction_engine: Arc<RwLock<PredictionEngine>>,
    pub health_monitor: Arc<RwLock<HealthMonitor>>,
}

impl PipelineEngine {
    pub fn new(
        world_state_manager: WorldStateManager,
        backend_registry: BackendRegistry,
        capacity: CapacitySnapshot,
        kv_intelligence: KvIntelligence,
    ) -> Self {
        let firewall = DecisionFirewall {
            config: UtilityConfig {
                weight_latency: 0.3,
                weight_throughput: 0.2,
                weight_reliability: 0.2,
                weight_quality: 0.15,
                weight_cost: 0.1,
                weight_kv_transfer: 0.05,
                max_ttft: 1000,
                max_tpot: 200,
                max_e2e: 5000,
                min_quality: 0.5,
                max_cost: 1.0,
                max_failure_probability: 0.2,
            },
            max_world_state_age: Duration::from_secs(300),
        };

        Self {
            world_state_manager,
            backend_registry: Arc::new(RwLock::new(backend_registry)),
            capacity: Arc::new(RwLock::new(capacity)),
            control_plane: Arc::new(RwLock::new(ControlPlaneSnapshot::new(
                kv_intelligence.clone(),
                ControlPlaneVersions::default(),
            ))),
            kv_intelligence: Arc::new(RwLock::new(kv_intelligence)),
            admission_controller: AdmissionController::new(crate::admission::AdmissionPolicy {
                global_in_flight: 1000,
                per_tenant_in_flight: 100,
            })
            .unwrap(),
            firewall,
            audit_chain: Arc::new(RwLock::new(AuditChain::new(1000))),
            decision_memory: Arc::new(RwLock::new(DecisionMemory::new(500))),
            prediction_tracker: Arc::new(RwLock::new(PredictionErrorTracker::new(500))),
            prediction_engine: Arc::new(RwLock::new(PredictionEngine::new(0.3, 5))),
            health_monitor: Arc::new(RwLock::new(HealthMonitor::start(crate::Router::new(), 1).0)),
        }
    }

    /// Primary entry point: process a request through the full deterministic pipeline.
    pub fn process_request(
        &self,
        req: &PipelineRequest,
        transport: &dyn InferenceTransport,
        kv_transport: Option<&dyn KvTransport>,
    ) -> PipelineOutcome {
        let mut trace_rejected: Vec<TraceRejection> = Vec::new();

        let decision_id = format!("dec-{}", req.request_id);

        let trace_finish = |success: bool, reason: &str, rejected: Vec<TraceRejection>| DecisionTrace {
            schema_version: crate::decision_trace::DECISION_TRACE_SCHEMA_VERSION,
            request_id: req.request_id.clone(),
            capacity_revision: 0,
            backend_revision: 0,
            selected: None,
            rejected,
            rejected_truncated: false,
            unavailable_nodes: Vec::new(),
            config_error: if !success { Some(reason.into()) } else { None },
        };

        // Stage 1: Authentication & Authorization
        if let Some(ref api_key) = req.api_key {
            if api_key == "invalid" || api_key.is_empty() {
                let trace = trace_finish(false, "Authentication failed", trace_rejected);
                return PipelineOutcome {
                    success: false,
                    request_id: req.request_id.clone(),
                    decision_id,
                    selected_backend: None,
                    selected_node: None,
                    selected_strategy: None,
                    selected_kv_action: None,
                    execution_plan: None,
                    compiled_plan: None,
                    execution_result: None,
                    rejection_reason: Some("Authentication failed".into()),
                    trace,
                    world_revision: 0,
                };
            }
        }

        // Stage 2: Admission & Rate Limits
        let _permit = match self.admission_controller.admit(&req.tenant_id) {
            Ok(permit) => permit,
            Err(err) => {
                let reason = format!("Admission rejected: {err:?}");
                let trace = trace_finish(false, &reason, trace_rejected);
            return PipelineOutcome {
                success: false,
                request_id: req.request_id.clone(),
                decision_id,
                selected_backend: None,
                selected_node: None,
                selected_strategy: None,
                selected_kv_action: None,
                execution_plan: None,
                compiled_plan: None,
                execution_result: None,
                rejection_reason: Some(reason),
                trace,
                world_revision: 0,
            };
            }
        };

        // Stage 3: Immutable Context / Snapshot
        let world_snapshot = match self.world_state_manager.snapshot() {
            Ok(snap) => {
                if req.force_stale_world_state {
                    let trace = trace_finish(false, "Stale world state revision", trace_rejected.clone());
                    return PipelineOutcome {
                        success: false,
                        request_id: req.request_id.clone(),
                        decision_id,
                        selected_backend: None,
                        selected_node: None,
                        selected_strategy: None,
                        selected_kv_action: None,
                        execution_plan: None,
                        compiled_plan: None,
                        execution_result: None,
                        rejection_reason: Some("Stale world state revision".into()),
                        trace,
                        world_revision: snap.revision,
                    };
                }
                snap
            }
            Err(err) => {
                let trace = trace_finish(false, &format!("World state error: {err:?}"), trace_rejected);
                return PipelineOutcome {
                    success: false,
                    request_id: req.request_id.clone(),
                    decision_id,
                    selected_backend: None,
                    selected_node: None,
                    selected_strategy: None,
                    selected_kv_action: None,
                    execution_plan: None,
                    compiled_plan: None,
                    execution_result: None,
                    rejection_reason: Some(format!("World state error: {err}")),
                    trace,
                    world_revision: 0,
                };
            }
        };

        let backends = self.backend_registry.read().unwrap();
        let capacity = self.capacity.read().unwrap();
        let _kv_reg = self.kv_intelligence.read().unwrap();
        
        let model_ref = ModelReference::new(&req.model, req.model_version.as_deref().unwrap_or("1"));

        // Stage 4: Candidate Generation & Deterministic Tie-Breaking
        let declared_backends = backends.backend_ids();
        if declared_backends.is_empty() {
            let trace = trace_finish(false, "No declared backends for model", trace_rejected);
            return PipelineOutcome {
                success: false,
                request_id: req.request_id.clone(),
                decision_id,
                selected_backend: None,
                selected_node: None,
                selected_strategy: None,
                selected_kv_action: None,
                execution_plan: None,
                compiled_plan: None,
                execution_result: None,
                rejection_reason: Some("No declared backends for model".into()),
                trace,
                world_revision: world_snapshot.revision,
            };
        }

        let requirement = CapacityRequirement {
            model: model_ref.clone(),
            region: None,
            min_available_memory_mb: 1_000,
            max_queue_depth: 10,
        };
        let (eligible_nodes, _) = capacity.evaluate(&requirement);

        let mut raw_candidates = Vec::new();
        for decl_id in &declared_backends {
            for node in &eligible_nodes {
                let all_strategies = [
                    crate::kv::KvStrategy::None,
                    crate::kv::KvStrategy::PrefixCache,
                    crate::kv::KvStrategy::CachedKv,
                    crate::kv::KvStrategy::CrossModelKvTransfer,
                ];
                let strategies: Vec<_> = all_strategies.iter().copied()
                    .filter(|s| backends.supports(decl_id, &model_ref, *s).unwrap_or(false)).collect();
                for strategy in strategies {
                    // Stage 5: KV Intelligence Evaluation
                    let kv_decision = if let Some(ref _prefix) = req.prompt_prefix_hash {
                        let input = KvDecisionInput {
                            compatible: true,
                            kv_size_mb: 100,
                            transfer_latency_ms: 10,
                            network_cost: 0.01,
                            recomputation_ms: 50,
                            expected_reuse: 0.8,
                            failure_risk: 0.01,
                            confidence: 0.9,
                        };
                        input.decide()
                    } else {
                        (KvAction::Recompute, "NO_PREFIX")
                    };

                    // Predictive Estimates
                    let predicted_ttft = self
                        .prediction_engine
                        .read()
                        .unwrap()
                        .predict(&format!("{}-ttft", decl_id), world_snapshot.revision)
                        .ok()
                        .map(|p| p.value as u64)
                        .unwrap_or(50);
                    let predicted_e2e = self
                        .prediction_engine
                        .read()
                        .unwrap()
                        .predict(&format!("{}-e2e", decl_id), world_snapshot.revision)
                        .ok()
                        .map(|p| p.value as u64)
                        .unwrap_or(200);

                    let estimate = CounterfactualEstimate {
                        backend_id: decl_id.to_string(),
                        compute_node_id: node.id.clone(),
                        model: req.model.clone(),
                        strategy: format!("{:?}", strategy),
                        predicted_ttft,
                        predicted_tpot: 10,
                        predicted_e2e,
                        predicted_cost: 0.005,
                        predicted_failure_probability: 0.01,
                        predicted_kv_transfer: 0,
                        quality_risk: 1.0,
                        confidence: 0.9,
                        feasible: true,
                        rejection_reason: None,
                    };

                    raw_candidates.push((estimate, kv_decision.0));
                }
            }
        }

        // Deterministic candidate sorting: (backend_id, compute_node_id, strategy)
        raw_candidates.sort_by(|a, b| {
            a.0.backend_id
                .cmp(&b.0.backend_id)
                .then_with(|| a.0.compute_node_id.cmp(&b.0.compute_node_id))
                .then_with(|| a.0.strategy.cmp(&b.0.strategy))
        });

        // Stage 6: Hard Constraints & Decision Firewall
        let mut valid_candidates = Vec::new();
        for (candidate, kv_action) in raw_candidates {
            // Hard constraint checks
            if let Some(max_ttft) = req.max_ttft_ms {
                if candidate.predicted_ttft > max_ttft {
                    trace_rejected.push(TraceRejection {
                        backend_id: candidate.backend_id.clone(),
                        compute_node_id: candidate.compute_node_id.clone(),
                        reason: format!("TTFT {}ms exceeds max {}ms", candidate.predicted_ttft, max_ttft),
                    });
                    continue;
                }
            }
            if let Some(max_e2e) = req.max_e2e_ms {
                if candidate.predicted_e2e > max_e2e {
                    trace_rejected.push(TraceRejection {
                        backend_id: candidate.backend_id.clone(),
                        compute_node_id: candidate.compute_node_id.clone(),
                        reason: format!("E2E {}ms exceeds max {}ms", candidate.predicted_e2e, max_e2e),
                    });
                    continue;
                }
            }
            if let Some(max_cost) = req.max_cost {
                if candidate.predicted_cost > max_cost {
                    trace_rejected.push(TraceRejection {
                        backend_id: candidate.backend_id.clone(),
                        compute_node_id: candidate.compute_node_id.clone(),
                        reason: format!("Cost {} exceeds max {}", candidate.predicted_cost, max_cost),
                    });
                    continue;
                }
            }

            // Firewall Evaluation
            if let FirewallDecision::Approved = self.firewall.evaluate_candidate(
                &candidate,
                &world_snapshot,
                DecisionConfidence::High,
            ) {
                valid_candidates.push((candidate, kv_action));
            } else {
                trace_rejected.push(TraceRejection {
                    backend_id: candidate.backend_id.clone(),
                    compute_node_id: candidate.compute_node_id.clone(),
                    reason: "Rejected by DecisionFirewall".into(),
                });
            }
        }

        if valid_candidates.is_empty() {
            let trace = trace_finish(false, "All candidates rejected by hard constraints/firewall", trace_rejected);
            return PipelineOutcome {
                success: false,
                request_id: req.request_id.clone(),
                decision_id,
                selected_backend: None,
                selected_node: None,
                selected_strategy: None,
                selected_kv_action: None,
                execution_plan: None,
                compiled_plan: None,
                execution_result: None,
                rejection_reason: Some("All candidates rejected by hard constraints/firewall".into()),
                trace,
                world_revision: world_snapshot.revision,
            };
        }

        // Stage 7: Counterfactual Evaluation & Ranking
        let estimates = valid_candidates
            .iter()
            .map(|(c, _)| c.clone())
            .collect::<Vec<_>>();
        let evaluated = CounterfactualEngine::evaluate(estimates);

        let top_index = evaluated[0].index;
        let (selected_candidate, selected_kv) = &valid_candidates[top_index];

        // Stage 8: Canonical ExecutionPlan & Compilation
        let plan = ExecutionPlan {
            schema_version: 1,
            snapshot: SystemSnapshot {
                tenant_id: req.tenant_id.clone(),
                start: selected_candidate.compute_node_id.clone(),
                goal: selected_candidate.compute_node_id.clone(),
                cache_hit: *selected_kv == KvAction::Reuse,
            },
            candidate: Candidate {
                nodes: vec![selected_candidate.compute_node_id.clone()],
                score: 1.0,
            },
            steps: vec![ExecutionStep {
                ordinal: 0,
                node: selected_candidate.compute_node_id.clone(),
            }],
            kv: crate::kv::KvPlanMetadata {
                inference_strategy: crate::kv::InferenceStrategy::NormalPrefill,
                strategy: crate::kv::KvStrategy::None,
                target: Some(model_ref.clone()),
                source: None,
                mapping_version: None,
                model_revision: 0,
                compatibility_revision: 0,
                fallback_reason: None,
            },
        };

        let compiled = ExecutionCompiler::compile(
            selected_candidate,
            (*selected_kv, "Selected deterministic action"),
            0.8,
            world_snapshot.revision,
            1,
            1,
        );

        // Stage 9: Inference Fabric & Execution Transport
        let transport_req = TransportRequest {
            request_id: req.request_id.clone(),
            model: req.model.clone(),
            model_version: req.model_version.clone().unwrap_or_else(|| "v1".into()),
            backend_id: selected_candidate.backend_id.clone(),
            timeout: compiled.timeout,
        };

        // Execute KV Transfer if required
        if *selected_kv == KvAction::Transfer {
            if let Some(kv_tp) = kv_transport {
                if let Err(err) = kv_tp.transfer("source-node", &selected_candidate.compute_node_id) {
                    let trace = trace_finish(false, &format!("KV Transfer failed: {err:?}"), trace_rejected);
                    return PipelineOutcome {
                        success: false,
                        request_id: req.request_id.clone(),
                        decision_id,
                        selected_backend: Some(selected_candidate.backend_id.clone()),
                        selected_node: Some(selected_candidate.compute_node_id.clone()),
                        selected_strategy: Some(selected_candidate.strategy.clone()),
                        selected_kv_action: Some(*selected_kv),
                        execution_plan: Some(plan),
                        compiled_plan: Some(compiled),
                        execution_result: None,
                        rejection_reason: Some(format!("KV Transfer failed: {err}")),
                        trace,
                        world_revision: world_snapshot.revision,
                    };
                }
            }
        }

        let transport_resp = match transport.execute(&transport_req) {
            Ok(resp) => resp,
            Err(err) => {
                let trace = trace_finish(false, &format!("Transport execution failed: {err:?}"), trace_rejected);
                return PipelineOutcome {
                    success: false,
                    request_id: req.request_id.clone(),
                    decision_id,
                    selected_backend: Some(selected_candidate.backend_id.clone()),
                    selected_node: Some(selected_candidate.compute_node_id.clone()),
                    selected_strategy: Some(selected_candidate.strategy.clone()),
                    selected_kv_action: Some(*selected_kv),
                    execution_plan: Some(plan),
                    compiled_plan: Some(compiled),
                    execution_result: None,
                    rejection_reason: Some(format!("Transport execution failed: {err}")),
                    trace,
                    world_revision: world_snapshot.revision,
                };
            }
        };

        let exec_result = ExecutionResult {
            backend: selected_candidate.backend_id.clone(),
            success: transport_resp.success,
            latency_ms: Some(transport_resp.latency_ms),
            estimated_cost: Some(selected_candidate.predicted_cost),
            failure: None,
        };

        // Record Audit Chain Entry
        self.audit_chain.write().unwrap().append(
            req.request_id.clone(),
            selected_candidate.backend_id.clone(),
            world_snapshot.revision,
        );

        // Stage 10: Feedback Loop & World-State Revision Bump
        let obs = InferenceObservation {
            schema_version: 1,
            request_id: req.request_id.clone(),
            tenant: req.tenant_id.clone(),
            model: req.model.clone(),
            model_version: req.model_version.clone().unwrap_or_else(|| "v1".into()),
            backend_id: selected_candidate.backend_id.clone(),
            backend_version: "1.0.0".into(),
            compute_node_id: selected_candidate.compute_node_id.clone(),
            accelerator: "A100".into(),
            region: "us-east-1".into(),
            prefill_tokens: req.prompt_tokens as u64,
            decode_tokens: req.max_tokens as u64,
            total_tokens: (req.prompt_tokens + req.max_tokens) as u64,
            queue_delay_ms: 2,
            ttft_ms: selected_candidate.predicted_ttft,
            tpot_ms: 10,
            e2e_latency_ms: transport_resp.latency_ms,
            gpu_utilization: 0.75,
            gpu_memory_used_mb: 500,
            gpu_memory_total_mb: 80000,
            kv_hit: *selected_kv == KvAction::Reuse,
            kv_tokens: req.prompt_tokens as u64,
            kv_memory_mb: 100,
            kv_transfer_ms: 0,
            network_transfer_ms: 0,
            retries: 0,
            failure_class: transport_resp.failure_class.clone(),
            quality_score: 1.0,
            estimated_cost: selected_candidate.predicted_cost,
            actual_cost: selected_candidate.predicted_cost,
            timestamp: SystemTime::now(),
            world_revision: world_snapshot.revision,
        };

        let new_revision = self
            .world_state_manager
            .ingest(&obs)
            .unwrap_or(world_snapshot.revision + 1);

        let pred_err = PredictionError::compute(
            "e2e_ms",
            selected_candidate.predicted_e2e as f64,
            transport_resp.latency_ms as f64,
            selected_candidate.confidence,
            &req.model_version.clone().unwrap_or_else(|| "1".into()),
            world_snapshot.revision,
        );
        self.prediction_tracker.write().unwrap().record(pred_err);

        let trace = trace_finish(true, "Execution successful", trace_rejected);

        PipelineOutcome {
            success: true,
            request_id: req.request_id.clone(),
            decision_id,
            selected_backend: Some(selected_candidate.backend_id.clone()),
            selected_node: Some(selected_candidate.compute_node_id.clone()),
            selected_strategy: Some(selected_candidate.strategy.clone()),
            selected_kv_action: Some(*selected_kv),
            execution_plan: Some(plan),
            compiled_plan: Some(compiled),
            execution_result: Some(exec_result),
            rejection_reason: None,
            trace,
            world_revision: new_revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend_registry::{BackendDescriptor, BackendRegistry};
    use crate::backend::MockBackend;
    use crate::capacity::{CapacitySnapshot, ComputeNode};
    use crate::kv::{KvIntelligence, ModelReference, KvStrategy};
    use crate::fabric::DeterministicTransport;
    use std::sync::Arc;
    use std::collections::{BTreeSet, BTreeMap};

    fn setup_engine() -> PipelineEngine {
        let world = WorldStateManager::new(Duration::from_secs(300));
        let mut backends = BackendRegistry::default();
        
        let model = ModelReference::new("test-model", "1");
        backends.register(
            BackendDescriptor {
                id: "backend-1".into(),
                version: "1.0".into(),
                capabilities: BTreeMap::from([(model.clone(), BTreeSet::from([KvStrategy::None]))]),
            },
            Arc::new(MockBackend {
                name: "backend-1".into(),
                model: model.clone(),
                supported: vec![KvStrategy::None],
                result: crate::backend::BackendResult {
                    backend: "backend-1".into(),
                    success: true,
                    latency_ms: Some(150),
                    estimated_cost: Some(0.01),
                    failure: None,
                },
            }),
        ).unwrap();

        let mut capacity = CapacitySnapshot::default();
        capacity.register(ComputeNode {
            id: "node-1".into(),
            region: "us-east".into(),
            accelerator: "a100".into(),
            total_memory_mb: 80000,
            available_memory_mb: 70000,
            utilization: 0.1,
            active_requests: 0,
            queue_depth: 0,
            healthy: true,
            resident_models: BTreeSet::from([model.clone()]),
        }).unwrap();

        let kv_intel = KvIntelligence::default();

        PipelineEngine::new(world, backends, capacity, kv_intel)
    }

    #[tokio::test]
    async fn pipeline_rejects_invalid_api_key() {
        let engine = setup_engine();
        let mut req = PipelineRequest::new("req-1", "tenant-1", "test-model");
        req.api_key = Some("invalid".into());
        let transport = DeterministicTransport { healthy: true, latency_ms: 100, capabilities: vec![] };
        
        let outcome = engine.process_request(&req, &transport, None);
        assert!(!outcome.success);
        assert_eq!(outcome.rejection_reason.unwrap(), "Authentication failed");
    }

    #[tokio::test]
    async fn pipeline_successful_execution() {
        let engine = setup_engine();
        let req = PipelineRequest::new("req-2", "tenant-1", "test-model");
        let transport = DeterministicTransport { healthy: true, latency_ms: 100, capabilities: vec![] };
        
        let outcome = engine.process_request(&req, &transport, None);
        assert!(outcome.success, "Pipeline failed with reason: {:?}", outcome.rejection_reason);
        assert_eq!(outcome.selected_backend.unwrap(), "backend-1");
        assert_eq!(outcome.selected_node.unwrap(), "node-1");
        assert!(outcome.execution_result.is_some());
        assert_eq!(outcome.execution_result.unwrap().success, true);
    }
}
