use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, MeterProvider};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use trust_router::planner::{PlanOutcome, Planner, PlanningRequest};
use trust_router::sidecar_config::{DEFAULT_TENANT, SidecarConfig};
use trust_router::sidecar_security::TokenBucket;
use trust_router::{
    AuditEvent, Edge, NodeState, RouteDecision, Router as TrustRouter, append_events_jsonl,
};

const TENANT: &str = DEFAULT_TENANT;
const KNOWN_NODES: [&str; 5] = [
    "start",
    "primary_search",
    "fallback_search",
    "summarize",
    "done",
];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_target(false).init();

    let args = std::env::args().collect::<Vec<_>>();
    let config = SidecarConfig::load(&args)?;
    let router = build_demo_router(&config)?;
    let recovery_loop =
        router.spawn_recovery_loop(config.health_policy.cooldown, config.recovery_scan_interval);
    let otel = OtelMetrics::from_env(&router);
    let metrics = Arc::new(Mutex::new(Metrics::default()));
    let state = AppState {
        router,
        audit_path: config.audit_path.clone(),
        api_key: config.api_key.clone(),
        allowed_tenants: config.allowed_tenants.iter().cloned().collect(),
        rate_limiter: Arc::new(Mutex::new(TokenBucket::new(&config.rate_limit))),
        next_request_id: AtomicU64::new(1),
        metrics,
        otel,
    };

    let app = AxumRouter::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics_handler))
        .route("/route", get(route_handler).post(route_handler))
        .route("/plan", get(plan_handler))
        .route("/result", post(result_handler))
        .route("/force-half-open", post(force_half_open_handler))
        .with_state(Arc::new(state));

    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    let local_addr = listener.local_addr()?;
    info!(%local_addr, "Trust Router sidecar listening");
    println!("Trust Router sidecar listening on http://{local_addr}");
    println!("Protected endpoints require X-API-Key.");
    println!("Audit file: {}", config.audit_path.display());
    println!("Try: http://{local_addr}/route?tenant=yc-demo&start=start&goal=done");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    recovery_loop.abort();
    let _ = recovery_loop.await;

    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        error!(%error, "failed to install shutdown signal handler");
    }
    info!("shutdown signal received");
}

struct AppState {
    router: TrustRouter,
    audit_path: PathBuf,
    api_key: String,
    allowed_tenants: HashSet<String>,
    rate_limiter: Arc<Mutex<TokenBucket>>,
    next_request_id: AtomicU64,
    metrics: Arc<Mutex<Metrics>>,
    otel: Option<Arc<OtelMetrics>>,
}

impl AppState {
    fn request_id(&self) -> String {
        format!(
            "tr-{:016x}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        )
    }
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn metrics_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let request_id = state.request_id();
    match authorize_request(&state, &headers, None) {
        Ok(()) => {
            let metrics = state.metrics.lock().expect("metrics lock poisoned");
            response_with_request_id(
                Json(metrics.to_response(&state.router)).into_response(),
                &request_id,
            )
        }
        Err(error) => error.into_response(&request_id),
    }
}

async fn route_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<RouteQuery>,
) -> Response {
    let request_id = state.request_id();
    match query
        .validate()
        .and_then(|_| authorize_request(&state, &headers, Some(&query.tenant)))
    {
        Ok(()) => match handle_route(&state, query) {
            Ok(response) => response_with_request_id(response.into_response(), &request_id),
            Err(error) => error.into_response(&request_id),
        },
        Err(error) => error.into_response(&request_id),
    }
}

async fn plan_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<RouteQuery>,
) -> Response {
    let request_id = state.request_id();
    match query
        .validate()
        .and_then(|_| authorize_request(&state, &headers, Some(&query.tenant)))
    {
        Ok(()) => match handle_plan(&state, query) {
            Ok(response) => response_with_request_id(response.into_response(), &request_id),
            Err(error) => error.into_response(&request_id),
        },
        Err(error) => error.into_response(&request_id),
    }
}

async fn result_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ResultQuery>,
) -> Response {
    let request_id = state.request_id();
    match query
        .validate()
        .and_then(|_| authorize_request(&state, &headers, Some(&query.tenant)))
    {
        Ok(()) => {
            state
                .router
                .record_result(&query.tenant, &query.node, query.success, query.latency_ms);
            if let Some(otel) = &state.otel {
                otel.record_node_health(&query.tenant, &state.router);
            }
            response_with_request_id(Json(OkResponse { ok: true }).into_response(), &request_id)
        }
        Err(error) => error.into_response(&request_id),
    }
}

async fn force_half_open_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ForceHalfOpenQuery>,
) -> Response {
    let request_id = state.request_id();
    match query
        .validate()
        .and_then(|_| authorize_request(&state, &headers, Some(&query.tenant)))
    {
        Ok(()) => {
            let changed = state.router.force_half_open(&query.tenant, &query.node);
            if changed && let Some(otel) = &state.otel {
                otel.record_node_health(&query.tenant, &state.router);
            }
            response_with_request_id(
                Json(ForceHalfOpenResponse { changed }).into_response(),
                &request_id,
            )
        }
        Err(error) => error.into_response(&request_id),
    }
}

fn handle_route(state: &AppState, query: RouteQuery) -> Result<Json<RouteResponse>, SidecarError> {
    let started = Instant::now();
    let decision = state.router.route(&query.tenant, &query.start, &query.goal);
    let elapsed = started.elapsed();
    let event = state
        .router
        .last_audit_event()
        .ok_or_else(|| SidecarError::Internal("route did not create an audit event".into()))?;
    persist_before_response(&state.audit_path, event)?;

    let response = RouteResponse::from_decision(decision.clone());
    let mut metrics = state.metrics.lock().expect("metrics lock poisoned");
    let route_metrics = metrics.record_route(&query.tenant, &decision, elapsed, &state.router);
    drop(metrics);
    if let Some(otel) = &state.otel {
        otel.record_route(
            &query.tenant,
            &decision,
            elapsed,
            route_metrics.false_escalation,
            route_metrics.rerouted,
            &state.router,
        );
    }
    Ok(Json(response))
}

fn handle_plan(state: &AppState, query: RouteQuery) -> Result<Json<PlanResponse>, SidecarError> {
    let started = Instant::now();
    let planner = Planner::new(state.router.clone());
    let outcome = planner
        .plan(PlanningRequest::new(
            &query.tenant,
            &query.start,
            &query.goal,
        ))
        .map_err(|error| SidecarError::BadRequest(error.to_string()))?;
    let event = state
        .router
        .last_audit_event()
        .ok_or_else(|| SidecarError::Internal("planner did not create an audit event".into()))?;
    persist_before_response(&state.audit_path, event)?;
    let elapsed = started.elapsed();
    let mut metrics = state.metrics.lock().expect("metrics lock poisoned");
    metrics.record_plan(&outcome, elapsed);
    drop(metrics);
    Ok(Json(PlanResponse::from_outcome(outcome)))
}

#[derive(Debug, Deserialize)]
struct RouteQuery {
    tenant: String,
    start: String,
    goal: String,
}

impl RouteQuery {
    fn validate(&self) -> Result<(), SidecarError> {
        validate_identifier("tenant", &self.tenant)?;
        validate_identifier("start", &self.start)?;
        validate_identifier("goal", &self.goal)?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ResultQuery {
    tenant: String,
    node: String,
    success: bool,
    latency_ms: u64,
}

impl ResultQuery {
    fn validate(&self) -> Result<(), SidecarError> {
        validate_identifier("tenant", &self.tenant)?;
        validate_identifier("node", &self.node)?;
        if self.latency_ms > 600_000 {
            return Err(SidecarError::BadRequest("latency_ms is too large".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ForceHalfOpenQuery {
    tenant: String,
    node: String,
}

impl ForceHalfOpenQuery {
    fn validate(&self) -> Result<(), SidecarError> {
        validate_identifier("tenant", &self.tenant)?;
        validate_identifier("node", &self.node)?;
        Ok(())
    }
}

fn validate_identifier(field: &str, value: &str) -> Result<(), SidecarError> {
    if value.is_empty() || value.len() > 128 {
        return Err(SidecarError::BadRequest(format!(
            "{field} must be 1-128 characters"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(SidecarError::BadRequest(format!(
            "{field} contains invalid characters"
        )));
    }
    Ok(())
}

#[derive(Debug)]
enum SidecarError {
    BadRequest(String),
    Unauthorized,
    Forbidden,
    RateLimited { retry_after_secs: u64 },
    Internal(String),
}

impl SidecarError {
    fn into_response(self, request_id: &str) -> Response {
        let (status, code, message, retry_after) = match self {
            SidecarError::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, "invalid_request", message, None)
            }
            SidecarError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing or invalid API key".into(),
                None,
            ),
            SidecarError::Forbidden => (
                StatusCode::FORBIDDEN,
                "tenant_not_authorized",
                "API key is not authorized for this tenant".into(),
                None,
            ),
            SidecarError::RateLimited { retry_after_secs } => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "request rate limit exceeded".into(),
                Some(retry_after_secs),
            ),
            SidecarError::Internal(message) => {
                error!(%request_id, %message, "sidecar internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal service error".into(),
                    None,
                )
            }
        };
        let mut response = (
            status,
            Json(ErrorResponse {
                error: ErrorBody {
                    code: code.to_string(),
                    message,
                    request_id: request_id.to_string(),
                },
            }),
        )
            .into_response();
        if let Some(seconds) = retry_after {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&seconds.to_string()).expect("retry-after is numeric"),
            );
        }
        response_with_request_id(response, request_id)
    }
}

fn authorize_request(
    state: &AppState,
    headers: &HeaderMap,
    tenant: Option<&str>,
) -> Result<(), SidecarError> {
    require_api_key(headers, &state.api_key)?;
    if let Some(tenant) = tenant
        && !state.allowed_tenants.contains(tenant)
    {
        return Err(SidecarError::Forbidden);
    }
    let mut limiter = state
        .rate_limiter
        .lock()
        .expect("rate limiter lock poisoned");
    limiter
        .try_acquire()
        .map_err(|error| SidecarError::RateLimited {
            retry_after_secs: error.retry_after.as_secs().max(1),
        })
}

fn response_with_request_id(mut response: Response, request_id: &str) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_str(request_id).expect("request ID only contains ASCII"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn require_api_key(headers: &HeaderMap, expected: &str) -> Result<(), SidecarError> {
    let Some(actual) = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
    else {
        return Err(SidecarError::Unauthorized);
    };
    constant_time_eq(actual.as_bytes(), expected.as_bytes())
        .then_some(())
        .ok_or(SidecarError::Unauthorized)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= (left_byte ^ right_byte) as usize;
    }
    diff == 0
}

fn persist_before_response(path: &PathBuf, event: AuditEvent) -> Result<(), SidecarError> {
    append_events_jsonl(path, &[event])
        .map(|_| ())
        .map_err(|error| SidecarError::Internal(format!("audit persistence failed: {error}")))
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: String,
    message: String,
    request_id: String,
}

#[derive(Debug, Serialize)]
struct ForceHalfOpenResponse {
    changed: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
enum RouteResponse {
    Routed {
        path: Vec<String>,
        total_cost: f64,
        cache_hit: bool,
    },
    Escalate {
        reason: String,
        failed_or_blocked_nodes: Vec<String>,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
enum PlanResponse {
    Execute {
        schema_version: u8,
        path: Vec<String>,
        total_cost: f64,
        cache_hit: bool,
        steps: Vec<String>,
    },
    Recover {
        reason: String,
        failed_or_blocked_nodes: Vec<String>,
        recovery_steps: Vec<String>,
    },
}

impl PlanResponse {
    fn from_outcome(outcome: PlanOutcome) -> Self {
        match outcome {
            PlanOutcome::Execute(plan) => Self::Execute {
                schema_version: plan.schema_version,
                path: plan.candidate.nodes,
                total_cost: plan.candidate.score,
                cache_hit: plan.snapshot.cache_hit,
                steps: plan.steps.into_iter().map(|step| step.node).collect(),
            },
            PlanOutcome::Recover {
                escalation,
                recovery,
                ..
            } => Self::Recover {
                reason: format!("{:?}", escalation.reason),
                failed_or_blocked_nodes: escalation.failed_or_blocked_nodes,
                recovery_steps: recovery.steps,
            },
        }
    }
}

impl RouteResponse {
    fn from_decision(decision: RouteDecision) -> Self {
        match decision {
            RouteDecision::Routed(route) => Self::Routed {
                path: route.nodes,
                total_cost: route.total_cost,
                cache_hit: route.cache_hit,
            },
            RouteDecision::Escalate(context) => Self::Escalate {
                reason: format!("{:?}", context.reason),
                failed_or_blocked_nodes: context.failed_or_blocked_nodes,
            },
        }
    }
}

const LATENCY_SAMPLE_LIMIT: usize = 4096;

#[derive(Debug, Default)]
struct Metrics {
    total_routes: u64,
    total_successful_routes: u64,
    total_reroutes: u64,
    total_escalations: u64,
    false_escalations: u64,
    cache_hits: u64,
    cache_misses: u64,
    per_tenant: HashMap<String, TenantMetrics>,
    route_latencies: BoundedLatencies,
    total_plans: u64,
    successful_plans: u64,
    recovery_plans: u64,
    plan_latencies: BoundedLatencies,
}

impl Metrics {
    fn record_route(
        &mut self,
        tenant: &str,
        decision: &RouteDecision,
        latency: Duration,
        router: &TrustRouter,
    ) -> RouteMetricOutcome {
        self.total_routes += 1;
        self.route_latencies.push(latency);
        let tenant_metrics = self.per_tenant.entry(tenant.to_string()).or_default();
        tenant_metrics.total_routes += 1;
        tenant_metrics.route_latencies.push(latency);

        let mut outcome = RouteMetricOutcome::default();
        match decision {
            RouteDecision::Routed(route) => {
                self.total_successful_routes += 1;
                tenant_metrics.total_successful_routes += 1;
                if route.cache_hit {
                    self.cache_hits += 1;
                    tenant_metrics.cache_hits += 1;
                } else {
                    self.cache_misses += 1;
                    tenant_metrics.cache_misses += 1;
                }

                let path_changed = tenant_metrics
                    .last_successful_path
                    .as_ref()
                    .is_some_and(|previous| previous != &route.nodes);
                if path_changed && !route.cache_hit {
                    self.total_reroutes += 1;
                    tenant_metrics.total_reroutes += 1;
                    outcome.rerouted = true;
                }
                tenant_metrics.last_successful_path = Some(route.nodes.clone());
            }
            RouteDecision::Escalate(_) => {
                self.total_escalations += 1;
                tenant_metrics.total_escalations += 1;
                outcome.false_escalation = penalty_path_available(router, tenant);
                if outcome.false_escalation {
                    self.false_escalations += 1;
                    tenant_metrics.false_escalations += 1;
                }
            }
        }
        outcome
    }

    fn record_plan(&mut self, outcome: &PlanOutcome, latency: Duration) {
        self.total_plans += 1;
        self.plan_latencies.push(latency);
        match outcome {
            PlanOutcome::Execute(_) => self.successful_plans += 1,
            PlanOutcome::Recover { .. } => self.recovery_plans += 1,
        }
    }

    fn to_response(&self, router: &TrustRouter) -> MetricsResponse {
        let tenants = self
            .per_tenant
            .iter()
            .map(|(tenant, metrics)| (tenant.clone(), metrics.to_response(router, tenant)))
            .collect::<HashMap<_, _>>();
        MetricsResponse {
            total_routes: self.total_routes,
            total_successful_routes: self.total_successful_routes,
            total_reroutes: self.total_reroutes,
            total_escalations: self.total_escalations,
            escalation_rate: rate(self.total_escalations, self.total_routes),
            llm_calls_avoided: self.total_routes.saturating_sub(self.total_escalations),
            false_escalations: self.false_escalations,
            false_escalation_rate: rate(self.false_escalations, self.total_routes),
            cache_hits: self.cache_hits,
            cache_misses: self.cache_misses,
            cache_hit_ratio: rate(self.cache_hits, self.cache_hits + self.cache_misses),
            node_health: node_health(router, TENANT),
            route_latency_us: self.route_latencies.percentiles(),
            total_plans: self.total_plans,
            successful_plans: self.successful_plans,
            recovery_plans: self.recovery_plans,
            plan_latency_us: self.plan_latencies.percentiles(),
            tenants,
        }
    }
}

#[derive(Debug, Default)]
struct RouteMetricOutcome {
    rerouted: bool,
    false_escalation: bool,
}

#[derive(Debug, Default)]
struct TenantMetrics {
    total_routes: u64,
    total_successful_routes: u64,
    total_reroutes: u64,
    total_escalations: u64,
    false_escalations: u64,
    cache_hits: u64,
    cache_misses: u64,
    last_successful_path: Option<Vec<String>>,
    route_latencies: BoundedLatencies,
}

impl TenantMetrics {
    fn to_response(&self, router: &TrustRouter, tenant: &str) -> TenantMetricsResponse {
        TenantMetricsResponse {
            total_routes: self.total_routes,
            total_successful_routes: self.total_successful_routes,
            total_reroutes: self.total_reroutes,
            total_escalations: self.total_escalations,
            escalation_rate: rate(self.total_escalations, self.total_routes),
            llm_calls_avoided: self.total_routes.saturating_sub(self.total_escalations),
            false_escalations: self.false_escalations,
            false_escalation_rate: rate(self.false_escalations, self.total_routes),
            cache_hits: self.cache_hits,
            cache_misses: self.cache_misses,
            cache_hit_ratio: rate(self.cache_hits, self.cache_hits + self.cache_misses),
            node_health: node_health(router, tenant),
            route_latency_us: self.route_latencies.percentiles(),
        }
    }
}

#[derive(Debug, Default)]
struct BoundedLatencies {
    values: VecDeque<Duration>,
}

impl BoundedLatencies {
    fn push(&mut self, value: Duration) {
        if self.values.len() == LATENCY_SAMPLE_LIMIT {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }

    fn percentiles(&self) -> LatencyPercentiles {
        let mut sorted = self.values.iter().copied().collect::<Vec<_>>();
        sorted.sort_unstable();
        LatencyPercentiles::from_sorted(&sorted)
    }
}

#[derive(Debug, Serialize)]
struct MetricsResponse {
    total_routes: u64,
    total_successful_routes: u64,
    total_reroutes: u64,
    total_escalations: u64,
    escalation_rate: f64,
    llm_calls_avoided: u64,
    false_escalations: u64,
    false_escalation_rate: f64,
    cache_hits: u64,
    cache_misses: u64,
    cache_hit_ratio: f64,
    node_health: HashMap<String, String>,
    route_latency_us: LatencyPercentiles,
    total_plans: u64,
    successful_plans: u64,
    recovery_plans: u64,
    plan_latency_us: LatencyPercentiles,
    tenants: HashMap<String, TenantMetricsResponse>,
}

#[derive(Debug, Serialize)]
struct TenantMetricsResponse {
    total_routes: u64,
    total_successful_routes: u64,
    total_reroutes: u64,
    total_escalations: u64,
    escalation_rate: f64,
    llm_calls_avoided: u64,
    false_escalations: u64,
    false_escalation_rate: f64,
    cache_hits: u64,
    cache_misses: u64,
    cache_hit_ratio: f64,
    node_health: HashMap<String, String>,
    route_latency_us: LatencyPercentiles,
}

#[derive(Debug, Serialize)]
struct LatencyPercentiles {
    p50: u128,
    p95: u128,
    p99: u128,
}

impl LatencyPercentiles {
    fn from_sorted(values: &[Duration]) -> Self {
        Self {
            p50: percentile_us(values, 50.0),
            p95: percentile_us(values, 95.0),
            p99: percentile_us(values, 99.0),
        }
    }
}

#[derive(Debug)]
struct OtelMetrics {
    _provider: SdkMeterProvider,
    route_total: Counter<u64>,
    reroute_total: Counter<u64>,
    escalation_total: Counter<u64>,
    false_escalation_total: Counter<u64>,
    llm_calls_avoided_total: Counter<u64>,
    route_latency_us: Histogram<f64>,
    node_health_state: Gauge<u64>,
}

impl OtelMetrics {
    fn from_env(router: &TrustRouter) -> Option<Arc<Self>> {
        if !matches!(
            std::env::var("TRUST_ROUTER_OTEL_STDOUT").as_deref(),
            Ok("1" | "true" | "TRUE" | "yes" | "YES")
        ) {
            return None;
        }

        let exporter = opentelemetry_stdout::MetricExporter::default();
        let reader = PeriodicReader::builder(exporter)
            .with_interval(otel_interval())
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("trust-router-sidecar");
        let metrics = Arc::new(Self {
            _provider: provider,
            route_total: meter.u64_counter("trust_router_routes_total").build(),
            reroute_total: meter.u64_counter("trust_router_reroutes_total").build(),
            escalation_total: meter.u64_counter("trust_router_escalations_total").build(),
            false_escalation_total: meter
                .u64_counter("trust_router_false_escalations_total")
                .build(),
            llm_calls_avoided_total: meter
                .u64_counter("trust_router_llm_calls_avoided_total")
                .build(),
            route_latency_us: meter
                .f64_histogram("trust_router_route_decision_latency_us")
                .build(),
            node_health_state: meter.u64_gauge("trust_router_node_health_state").build(),
        });
        metrics.record_node_health(TENANT, router);
        info!("OpenTelemetry stdout metrics export enabled");
        Some(metrics)
    }

    fn record_route(
        &self,
        tenant: &str,
        decision: &RouteDecision,
        latency: Duration,
        false_escalation: bool,
        rerouted: bool,
        router: &TrustRouter,
    ) {
        let attrs = [KeyValue::new("tenant", tenant.to_string())];
        self.route_total.add(1, &attrs);
        self.route_latency_us
            .record(latency.as_micros() as f64, &attrs);
        match decision {
            RouteDecision::Routed(_) => {
                if rerouted {
                    self.reroute_total.add(1, &attrs);
                }
                self.llm_calls_avoided_total.add(1, &attrs);
            }
            RouteDecision::Escalate(_) => {
                self.escalation_total.add(1, &attrs);
                if false_escalation {
                    self.false_escalation_total.add(1, &attrs);
                }
            }
        }
        self.record_node_health(tenant, router);
    }

    fn record_node_health(&self, tenant: &str, router: &TrustRouter) {
        for node in KNOWN_NODES {
            let state = router
                .node_state(tenant, node)
                .unwrap_or(NodeState::Healthy);
            self.node_health_state.record(
                node_state_value(state),
                &[
                    KeyValue::new("tenant", tenant.to_string()),
                    KeyValue::new("node", node.to_string()),
                    KeyValue::new("state", format!("{state:?}")),
                ],
            );
        }
    }
}

fn otel_interval() -> Duration {
    std::env::var("TRUST_ROUTER_OTEL_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(60))
}

fn node_state_value(state: NodeState) -> u64 {
    match state {
        NodeState::Healthy => 0,
        NodeState::Degraded => 1,
        NodeState::HalfOpen => 2,
        NodeState::Open => 3,
    }
}

fn rate(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn percentile_us(values: &[Duration], percentile: f64) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let index = ((values.len() - 1) as f64 * percentile / 100.0).round() as usize;
    values[index].as_micros()
}

fn node_health(router: &TrustRouter, tenant: &str) -> HashMap<String, String> {
    KNOWN_NODES
        .iter()
        .map(|node| {
            let state = router
                .node_state(tenant, node)
                .map(|state| format!("{state:?}"))
                .unwrap_or_else(|| "Healthy".to_string());
            ((*node).to_string(), state)
        })
        .collect()
}

fn penalty_path_available(router: &TrustRouter, tenant: &str) -> bool {
    KNOWN_NODES.iter().any(|node| {
        router
            .node_state(tenant, node)
            .map(|state| matches!(state, NodeState::Degraded | NodeState::HalfOpen))
            .unwrap_or(false)
    })
}

fn build_demo_router(config: &SidecarConfig) -> Result<TrustRouter, trust_router::RouterError> {
    let router = TrustRouter::new();
    router.add_tenant(TENANT);
    router.try_set_cost_model(TENANT, config.cost_model)?;
    router.try_set_health_policy(TENANT, config.health_policy)?;
    router.add_edge(
        TENANT,
        Edge::new("start", "primary_search").with_costs(1.0, 0.001, 120, 0.1),
    );
    router.add_edge(
        TENANT,
        Edge::new("primary_search", "summarize").with_costs(1.0, 0.002, 320, 0.2),
    );
    router.add_edge(
        TENANT,
        Edge::new("start", "fallback_search").with_costs(10.0, 0.01, 1_000, 1.0),
    );
    router.add_edge(
        TENANT,
        Edge::new("fallback_search", "summarize").with_costs(5.0, 0.005, 800, 0.5),
    );
    router.add_edge(
        TENANT,
        Edge::new("summarize", "done").with_costs(1.0, 0.001, 120, 0.1),
    );
    Ok(router)
}
