use std::env;
use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::{CostCeilings, CostWeights, HealthPolicy, RouterError, TenantCostModel};

pub const DEFAULT_ADDRESS: &str = "127.0.0.1:7878";
pub const DEFAULT_AUDIT_PATH: &str = "sidecar-audit.jsonl";
pub const DEFAULT_DEMO_API_KEY: &str = "trust-router-demo-key";
pub const DEFAULT_TENANT: &str = "yc-demo";

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    #[default]
    Development,
    Production,
}

#[derive(Clone)]
pub struct SidecarConfig {
    pub mode: DeploymentMode,
    pub bind_address: SocketAddr,
    pub audit_path: PathBuf,
    pub api_key: String,
    pub allowed_tenants: Vec<String>,
    pub rate_limit: RateLimitConfig,
    pub cost_model: TenantCostModel,
    pub health_policy: HealthPolicy,
    pub recovery_scan_interval: Duration,
}

impl fmt::Debug for SidecarConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SidecarConfig")
            .field("mode", &self.mode)
            .field("bind_address", &self.bind_address)
            .field("audit_path", &self.audit_path)
            .field("api_key", &"[redacted]")
            .field("allowed_tenants", &self.allowed_tenants)
            .field("rate_limit", &self.rate_limit)
            .field("cost_model", &self.cost_model)
            .field("health_policy", &self.health_policy)
            .field("recovery_scan_interval", &self.recovery_scan_interval)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    mode: DeploymentMode,
    bind_address: Option<String>,
    audit_path: Option<PathBuf>,
    api_key: Option<String>,
    api_key_file: Option<PathBuf>,
    allowed_tenants: Vec<String>,
    rate_limit: RateLimitConfig,
    cost: RawCostModel,
    health: RawHealthPolicy,
    recovery_scan_interval_ms: u64,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            mode: DeploymentMode::Development,
            bind_address: None,
            audit_path: None,
            api_key: None,
            api_key_file: None,
            allowed_tenants: vec![DEFAULT_TENANT.to_string()],
            rate_limit: RateLimitConfig::default(),
            cost: RawCostModel::default(),
            health: RawHealthPolicy::default(),
            recovery_scan_interval_ms: 1_000,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RateLimitConfig {
    pub capacity: u32,
    pub refill_per_second: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            capacity: 120,
            refill_per_second: 60,
        }
    }
}

impl RateLimitConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.capacity == 0 || self.refill_per_second == 0 {
            return Err(ConfigError::Invalid(
                "rate_limit.capacity and rate_limit.refill_per_second must be greater than zero"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawCostModel {
    w_dollar: f64,
    w_latency: f64,
    w_risk: f64,
    w_base: f64,
    max_dollar: f64,
    max_latency_ms: u64,
    max_base: f64,
}

impl Default for RawCostModel {
    fn default() -> Self {
        let model = TenantCostModel::default();
        Self {
            w_dollar: model.weights.w_dollar,
            w_latency: model.weights.w_latency,
            w_risk: model.weights.w_risk,
            w_base: model.weights.w_base,
            max_dollar: model.ceilings.max_dollar,
            max_latency_ms: model.ceilings.max_latency_ms,
            max_base: model.ceilings.max_base,
        }
    }
}

impl RawCostModel {
    fn into_model(self) -> TenantCostModel {
        TenantCostModel {
            weights: CostWeights {
                w_dollar: self.w_dollar,
                w_latency: self.w_latency,
                w_risk: self.w_risk,
                w_base: self.w_base,
            },
            ceilings: CostCeilings {
                max_dollar: self.max_dollar,
                max_latency_ms: self.max_latency_ms,
                max_base: self.max_base,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawHealthPolicy {
    open_threshold: u32,
    degraded_threshold: u32,
    half_open_success_needed: u32,
    degraded_success_needed: u32,
    cooldown_ms: u64,
}

impl Default for RawHealthPolicy {
    fn default() -> Self {
        let policy = HealthPolicy::default();
        Self {
            open_threshold: policy.open_threshold,
            degraded_threshold: policy.degraded_threshold,
            half_open_success_needed: policy.half_open_success_needed,
            degraded_success_needed: policy.degraded_success_needed,
            cooldown_ms: policy.cooldown.as_millis() as u64,
        }
    }
}

impl RawHealthPolicy {
    fn into_policy(self) -> HealthPolicy {
        HealthPolicy {
            open_threshold: self.open_threshold,
            degraded_threshold: self.degraded_threshold,
            half_open_success_needed: self.half_open_success_needed,
            degraded_success_needed: self.degraded_success_needed,
            cooldown: Duration::from_millis(self.cooldown_ms),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    ParseFile {
        path: PathBuf,
        source: serde_json::Error,
    },
    Invalid(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFile { path, source } => write!(
                formatter,
                "cannot read config file {}: {source}",
                path.display()
            ),
            Self::ParseFile { path, source } => write!(
                formatter,
                "cannot parse config file {}: {source}",
                path.display()
            ),
            Self::Invalid(message) => write!(formatter, "invalid sidecar configuration: {message}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<RouterError> for ConfigError {
    fn from(error: RouterError) -> Self {
        Self::Invalid(error.to_string())
    }
}

impl SidecarConfig {
    pub fn load(args: &[String]) -> Result<Self, ConfigError> {
        let raw = match env::var_os("TRUST_ROUTER_CONFIG") {
            Some(path) => {
                let path = PathBuf::from(path);
                let contents =
                    fs::read_to_string(&path).map_err(|source| ConfigError::ReadFile {
                        path: path.clone(),
                        source,
                    })?;
                serde_json::from_str(&contents)
                    .map_err(|source| ConfigError::ParseFile { path, source })?
            }
            None => RawConfig::default(),
        };
        Self::from_raw(raw, args)
    }

    fn from_raw(mut raw: RawConfig, args: &[String]) -> Result<Self, ConfigError> {
        if let Ok(value) = env::var("TRUST_ROUTER_MODE") {
            raw.mode = parse_mode(&value)?;
        }
        if let Ok(value) = env::var("TRUST_ROUTER_BIND_ADDRESS") {
            raw.bind_address = Some(value);
        }
        if let Ok(value) = env::var("TRUST_ROUTER_AUDIT_PATH") {
            raw.audit_path = Some(PathBuf::from(value));
        }
        if let Ok(value) = env::var("TRUST_ROUTER_ALLOWED_TENANTS") {
            raw.allowed_tenants = split_tenants(&value)?;
        }
        override_u32(
            "TRUST_ROUTER_RATE_LIMIT_CAPACITY",
            &mut raw.rate_limit.capacity,
        )?;
        override_u32(
            "TRUST_ROUTER_RATE_LIMIT_REFILL_PER_SECOND",
            &mut raw.rate_limit.refill_per_second,
        )?;
        override_f64("TRUST_ROUTER_W_DOLLAR", &mut raw.cost.w_dollar)?;
        override_f64("TRUST_ROUTER_W_LATENCY", &mut raw.cost.w_latency)?;
        override_f64("TRUST_ROUTER_W_RISK", &mut raw.cost.w_risk)?;
        override_f64("TRUST_ROUTER_W_BASE", &mut raw.cost.w_base)?;
        override_f64("TRUST_ROUTER_MAX_DOLLAR", &mut raw.cost.max_dollar)?;
        override_u64("TRUST_ROUTER_MAX_LATENCY_MS", &mut raw.cost.max_latency_ms)?;
        override_f64("TRUST_ROUTER_MAX_BASE", &mut raw.cost.max_base)?;
        override_u32(
            "TRUST_ROUTER_OPEN_THRESHOLD",
            &mut raw.health.open_threshold,
        )?;
        override_u32(
            "TRUST_ROUTER_DEGRADED_THRESHOLD",
            &mut raw.health.degraded_threshold,
        )?;
        override_u32(
            "TRUST_ROUTER_HALF_OPEN_SUCCESS_NEEDED",
            &mut raw.health.half_open_success_needed,
        )?;
        override_u32(
            "TRUST_ROUTER_DEGRADED_SUCCESS_NEEDED",
            &mut raw.health.degraded_success_needed,
        )?;
        override_u64("TRUST_ROUTER_COOLDOWN_MS", &mut raw.health.cooldown_ms)?;
        override_u64(
            "TRUST_ROUTER_RECOVERY_SCAN_INTERVAL_MS",
            &mut raw.recovery_scan_interval_ms,
        )?;

        let bind_source = args
            .get(1)
            .cloned()
            .or_else(|| raw.bind_address.clone())
            .or_else(|| env::var("PORT").ok().map(|port| format!("0.0.0.0:{port}")))
            .unwrap_or_else(|| DEFAULT_ADDRESS.to_string());
        let bind_address = bind_source.parse().map_err(|_| {
            ConfigError::Invalid(format!(
                "bind_address must be a socket address, got {bind_source:?}"
            ))
        })?;
        let audit_path = args
            .get(2)
            .map(PathBuf::from)
            .or_else(|| raw.audit_path.clone())
            .unwrap_or_else(|| PathBuf::from(DEFAULT_AUDIT_PATH));

        let api_key = api_key_from_sources(&raw)?;
        validate_api_key(raw.mode, &api_key)?;
        raw.rate_limit.validate()?;
        if raw.recovery_scan_interval_ms == 0 {
            return Err(ConfigError::Invalid(
                "recovery_scan_interval_ms must be greater than zero".into(),
            ));
        }
        if raw.allowed_tenants.is_empty() {
            return Err(ConfigError::Invalid(
                "allowed_tenants cannot be empty".into(),
            ));
        }
        for tenant in &raw.allowed_tenants {
            validate_identifier("allowed_tenants entry", tenant)?;
        }

        let cost_model = raw.cost.into_model();
        cost_model.validate()?;
        let health_policy = raw.health.into_policy();
        health_policy.validate()?;

        Ok(Self {
            mode: raw.mode,
            bind_address,
            audit_path,
            api_key,
            allowed_tenants: raw.allowed_tenants,
            rate_limit: raw.rate_limit,
            cost_model,
            health_policy,
            recovery_scan_interval: Duration::from_millis(raw.recovery_scan_interval_ms),
        })
    }
}

fn api_key_from_sources(raw: &RawConfig) -> Result<String, ConfigError> {
    if let Ok(value) = env::var("TRUST_ROUTER_API_KEY") {
        return Ok(value);
    }
    if let Some(path) = env::var_os("TRUST_ROUTER_API_KEY_FILE")
        .map(PathBuf::from)
        .or_else(|| raw.api_key_file.clone())
    {
        return fs::read_to_string(&path)
            .map(|value| value.trim_end_matches(['\r', '\n']).to_string())
            .map_err(|source| ConfigError::ReadFile { path, source });
    }
    if let Some(value) = &raw.api_key {
        return Ok(value.clone());
    }
    if raw.mode == DeploymentMode::Development {
        return Ok(DEFAULT_DEMO_API_KEY.to_string());
    }
    Err(ConfigError::Invalid(
        "production mode requires TRUST_ROUTER_API_KEY or TRUST_ROUTER_API_KEY_FILE".into(),
    ))
}

fn validate_api_key(mode: DeploymentMode, key: &str) -> Result<(), ConfigError> {
    if key.is_empty() {
        return Err(ConfigError::Invalid("API key cannot be empty".into()));
    }
    if key.len() > 512 || key.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ConfigError::Invalid(
            "API key must be at most 512 printable characters".into(),
        ));
    }
    if mode == DeploymentMode::Production && (key == DEFAULT_DEMO_API_KEY || key.len() < 32) {
        return Err(ConfigError::Invalid(
            "production API key must be a non-demo secret of at least 32 characters".into(),
        ));
    }
    Ok(())
}

fn parse_mode(value: &str) -> Result<DeploymentMode, ConfigError> {
    match value {
        "development" => Ok(DeploymentMode::Development),
        "production" => Ok(DeploymentMode::Production),
        _ => Err(ConfigError::Invalid(
            "TRUST_ROUTER_MODE must be development or production".into(),
        )),
    }
}

fn split_tenants(value: &str) -> Result<Vec<String>, ConfigError> {
    let tenants = value
        .split(',')
        .map(str::trim)
        .filter(|tenant| !tenant.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if tenants.is_empty() {
        return Err(ConfigError::Invalid(
            "TRUST_ROUTER_ALLOWED_TENANTS cannot be empty".into(),
        ));
    }
    Ok(tenants)
}

fn validate_identifier(field: &str, value: &str) -> Result<(), ConfigError> {
    if value.is_empty() || value.len() > 128 {
        return Err(ConfigError::Invalid(format!(
            "{field} must be 1-128 characters"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(ConfigError::Invalid(format!(
            "{field} contains invalid characters"
        )));
    }
    Ok(())
}

fn override_u32(name: &str, target: &mut u32) -> Result<(), ConfigError> {
    if let Ok(value) = env::var(name) {
        *target = value
            .parse()
            .map_err(|_| ConfigError::Invalid(format!("{name} must be an unsigned integer")))?;
    }
    Ok(())
}

fn override_u64(name: &str, target: &mut u64) -> Result<(), ConfigError> {
    if let Ok(value) = env::var(name) {
        *target = value
            .parse()
            .map_err(|_| ConfigError::Invalid(format!("{name} must be an unsigned integer")))?;
    }
    Ok(())
}

fn override_f64(name: &str, target: &mut f64) -> Result<(), ConfigError> {
    if let Ok(value) = env::var(name) {
        *target = value
            .parse()
            .map_err(|_| ConfigError::Invalid(format!("{name} must be a number")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_production_configuration() {
        let mut raw = RawConfig {
            mode: DeploymentMode::Production,
            ..RawConfig::default()
        };
        raw.api_key = Some(DEFAULT_DEMO_API_KEY.to_string());

        let error = SidecarConfig::from_raw(raw, &["sidecar".to_string()])
            .expect_err("demo API key must not be accepted in production");
        assert!(error.to_string().contains("production API key"));
    }

    #[test]
    fn rejects_invalid_cost_and_health_values() {
        let mut raw = RawConfig::default();
        raw.cost.w_dollar = f64::NAN;
        let error = SidecarConfig::from_raw(raw, &["sidecar".to_string()])
            .expect_err("NaN weight must fail startup");
        assert!(error.to_string().contains("invalid cost model"));

        let mut raw = RawConfig::default();
        raw.health.open_threshold = 0;
        let error = SidecarConfig::from_raw(raw, &["sidecar".to_string()])
            .expect_err("zero threshold must fail startup");
        assert!(error.to_string().contains("invalid health policy"));
    }
}
