//! Backend-agnostic KV strategy metadata. Unknown measurements never imply reuse.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelReference {
    pub model_id: String,
    pub model_version: String,
}
impl ModelReference {
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            model_id: id.into(),
            model_version: version.into(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceStrategy {
    NormalPrefill,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KvStrategy {
    None,
    PrefixCache,
    CachedKv,
    CrossModelKvTransfer,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub kv_strategies: BTreeSet<KvStrategy>,
}
impl ModelCapabilities {
    pub fn normal_prefill() -> Self {
        Self {
            kv_strategies: BTreeSet::new(),
        }
    }
    pub fn with_kv(mut self, value: KvStrategy) -> Self {
        if value != KvStrategy::None {
            self.kv_strategies.insert(value);
        }
        self
    }
    pub fn supports(&self, value: KvStrategy) -> bool {
        value == KvStrategy::None || self.kv_strategies.contains(&value)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub model: ModelReference,
    pub kv_layout: String,
    pub capabilities: ModelCapabilities,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KvError {
    Invalid(String),
    DuplicateModel(ModelReference),
    UnknownModel(String),
    UnknownVersion(ModelReference),
    DuplicateCompatibility,
}
impl fmt::Display for KvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for KvError {}
fn valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    models: BTreeMap<ModelReference, ModelMetadata>,
    revision: u64,
}
impl ModelRegistry {
    pub fn register(&mut self, m: ModelMetadata) -> Result<(), KvError> {
        if !valid(&m.model.model_id)
            || !valid(&m.model.model_version)
            || !valid(&m.kv_layout)
            || m.capabilities.kv_strategies.contains(&KvStrategy::None)
        {
            return Err(KvError::Invalid("invalid model metadata".into()));
        }
        if self.models.contains_key(&m.model) {
            return Err(KvError::DuplicateModel(m.model));
        }
        self.models.insert(m.model.clone(), m);
        self.revision += 1;
        Ok(())
    }
    pub fn lookup(&self, m: &ModelReference) -> Result<&ModelMetadata, KvError> {
        self.models.get(m).ok_or_else(|| {
            if self.models.keys().any(|x| x.model_id == m.model_id) {
                KvError::UnknownVersion(m.clone())
            } else {
                KvError::UnknownModel(m.model_id.clone())
            }
        })
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvCompatibilityRecord {
    pub source: ModelReference,
    pub target: ModelReference,
    pub supported: bool,
    pub mapping_version: Option<String>,
    pub quality_retention: Option<f64>,
    pub quality_variance: Option<f64>,
    pub transfer_latency_p50_ms: Option<f64>,
    pub transfer_latency_p95_ms: Option<f64>,
    pub failure_rate: Option<f64>,
    pub sample_count: Option<u64>,
    pub confidence: Option<f64>,
}
impl KvCompatibilityRecord {
    pub fn unknown(source: ModelReference, target: ModelReference) -> Self {
        Self {
            source,
            target,
            supported: false,
            mapping_version: None,
            quality_retention: None,
            quality_variance: None,
            transfer_latency_p50_ms: None,
            transfer_latency_p95_ms: None,
            failure_rate: None,
            sample_count: None,
            confidence: None,
        }
    }
    fn validate(&self) -> Result<(), KvError> {
        if self.source == self.target
            || !valid(&self.source.model_id)
            || !valid(&self.source.model_version)
            || !valid(&self.target.model_id)
            || !valid(&self.target.model_version)
        {
            return Err(KvError::Invalid("invalid source or target model".into()));
        }
        if self.supported && self.mapping_version.is_none() {
            return Err(KvError::Invalid(
                "supported transfer requires a mapping version".into(),
            ));
        }
        if self
            .mapping_version
            .as_ref()
            .is_some_and(|value| !valid(value))
        {
            return Err(KvError::Invalid("invalid mapping version".into()));
        }
        for value in [self.quality_retention, self.failure_rate, self.confidence]
            .into_iter()
            .flatten()
        {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(KvError::Invalid("invalid ratio measurement".into()));
            }
        }
        for value in [
            self.quality_variance,
            self.transfer_latency_p50_ms,
            self.transfer_latency_p95_ms,
        ]
        .into_iter()
        .flatten()
        {
            if !value.is_finite() || value < 0.0 {
                return Err(KvError::Invalid("invalid non-negative measurement".into()));
            }
        }
        if self.sample_count == Some(0) {
            return Err(KvError::Invalid("sample_count must be positive".into()));
        }
        if let (Some(p50), Some(p95)) = (self.transfer_latency_p50_ms, self.transfer_latency_p95_ms)
            && p95 < p50
        {
            return Err(KvError::Invalid("transfer p95 cannot be below p50".into()));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default)]
pub struct KvCompatibilityRegistry {
    records: BTreeMap<(ModelReference, ModelReference), KvCompatibilityRecord>,
    revision: u64,
}
impl KvCompatibilityRegistry {
    pub fn register(&mut self, r: KvCompatibilityRecord) -> Result<(), KvError> {
        r.validate()?;
        let key = (r.source.clone(), r.target.clone());
        if self.records.contains_key(&key) {
            return Err(KvError::DuplicateCompatibility);
        }
        self.records.insert(key, r);
        self.revision += 1;
        Ok(())
    }
    pub fn lookup(&self, s: &ModelReference, t: &ModelReference) -> Option<&KvCompatibilityRecord> {
        self.records.get(&(s.clone(), t.clone()))
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KvPolicy {
    pub min_quality_retention: f64,
    pub max_quality_variance: f64,
    pub min_confidence: f64,
    pub max_failure_rate: f64,
}
impl Default for KvPolicy {
    fn default() -> Self {
        Self {
            min_quality_retention: 0.95,
            max_quality_variance: 0.02,
            min_confidence: 0.95,
            max_failure_rate: 0.01,
        }
    }
}
impl KvPolicy {
    pub fn validate(self) -> Result<(), KvError> {
        for value in [
            self.min_quality_retention,
            self.max_quality_variance,
            self.min_confidence,
            self.max_failure_rate,
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(KvError::Invalid("invalid KV policy ratio".into()));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvPlanningContext {
    pub target: ModelReference,
    pub source: Option<ModelReference>,
    pub requested: KvStrategy,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KvFallbackReason {
    UnknownModel,
    UnknownVersion,
    UnknownSourceModel,
    UnknownSourceVersion,
    UnsupportedStrategy,
    MissingSource,
    CompatibilityUnknown,
    CompatibilityNotSupported,
    MeasurementsUnknown,
    InsufficientQuality,
    ExcessQualityVariance,
    InsufficientConfidence,
    UnreliableTransfer,
    InvalidPolicy,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvPlanMetadata {
    pub inference_strategy: InferenceStrategy,
    pub strategy: KvStrategy,
    pub target: Option<ModelReference>,
    pub source: Option<ModelReference>,
    pub mapping_version: Option<String>,
    pub model_revision: u64,
    pub compatibility_revision: u64,
    pub fallback_reason: Option<KvFallbackReason>,
}
#[derive(Debug, Clone, Default)]
pub struct KvIntelligence {
    pub models: ModelRegistry,
    pub compatibility: KvCompatibilityRegistry,
    pub policy: KvPolicy,
}
impl KvIntelligence {
    pub fn normal(&self) -> KvPlanMetadata {
        KvPlanMetadata {
            inference_strategy: InferenceStrategy::NormalPrefill,
            strategy: KvStrategy::None,
            target: None,
            source: None,
            mapping_version: None,
            model_revision: self.models.revision(),
            compatibility_revision: self.compatibility.revision(),
            fallback_reason: None,
        }
    }
    pub fn select(&self, c: &KvPlanningContext) -> KvPlanMetadata {
        let fallback = |reason| KvPlanMetadata {
            inference_strategy: InferenceStrategy::NormalPrefill,
            strategy: KvStrategy::None,
            target: Some(c.target.clone()),
            source: c.source.clone(),
            mapping_version: None,
            model_revision: self.models.revision(),
            compatibility_revision: self.compatibility.revision(),
            fallback_reason: Some(reason),
        };
        if self.policy.validate().is_err() {
            return fallback(KvFallbackReason::InvalidPolicy);
        }
        let target = match self.models.lookup(&c.target) {
            Ok(v) => v,
            Err(KvError::UnknownVersion(_)) => return fallback(KvFallbackReason::UnknownVersion),
            Err(_) => return fallback(KvFallbackReason::UnknownModel),
        };
        if c.requested == KvStrategy::None {
            return self.normal();
        }
        if !target.capabilities.supports(c.requested) {
            return fallback(KvFallbackReason::UnsupportedStrategy);
        }
        if c.requested != KvStrategy::CrossModelKvTransfer {
            return KvPlanMetadata {
                inference_strategy: InferenceStrategy::NormalPrefill,
                strategy: c.requested,
                target: Some(c.target.clone()),
                source: c.source.clone(),
                mapping_version: None,
                model_revision: self.models.revision(),
                compatibility_revision: self.compatibility.revision(),
                fallback_reason: None,
            };
        }
        let Some(source) = &c.source else {
            return fallback(KvFallbackReason::MissingSource);
        };
        let source_metadata = match self.models.lookup(source) {
            Ok(model) => model,
            Err(KvError::UnknownVersion(_)) => {
                return fallback(KvFallbackReason::UnknownSourceVersion);
            }
            Err(_) => return fallback(KvFallbackReason::UnknownSourceModel),
        };
        if !source_metadata.capabilities.supports(c.requested) {
            return fallback(KvFallbackReason::UnsupportedStrategy);
        }
        let Some(r) = self.compatibility.lookup(source, &c.target) else {
            return fallback(KvFallbackReason::CompatibilityUnknown);
        };
        if !r.supported {
            return fallback(KvFallbackReason::CompatibilityNotSupported);
        }
        let (Some(map), Some(q), Some(variance), Some(f), Some(n), Some(conf), Some(_), Some(_)) = (
            r.mapping_version.clone(),
            r.quality_retention,
            r.quality_variance,
            r.failure_rate,
            r.sample_count,
            r.confidence,
            r.transfer_latency_p50_ms,
            r.transfer_latency_p95_ms,
        ) else {
            return fallback(KvFallbackReason::MeasurementsUnknown);
        };
        if n == 0 {
            return fallback(KvFallbackReason::MeasurementsUnknown);
        }
        if q < self.policy.min_quality_retention {
            return fallback(KvFallbackReason::InsufficientQuality);
        }
        if variance > self.policy.max_quality_variance {
            return fallback(KvFallbackReason::ExcessQualityVariance);
        }
        if conf < self.policy.min_confidence {
            return fallback(KvFallbackReason::InsufficientConfidence);
        }
        if f > self.policy.max_failure_rate {
            return fallback(KvFallbackReason::UnreliableTransfer);
        }
        KvPlanMetadata {
            inference_strategy: InferenceStrategy::NormalPrefill,
            strategy: c.requested,
            target: Some(c.target.clone()),
            source: Some(source.clone()),
            mapping_version: Some(map),
            model_revision: self.models.revision(),
            compatibility_revision: self.compatibility.revision(),
            fallback_reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured_record() -> KvCompatibilityRecord {
        KvCompatibilityRecord {
            source: ModelReference::new("source", "1"),
            target: ModelReference::new("target", "1"),
            supported: true,
            mapping_version: Some("mapping-1".into()),
            quality_retention: Some(0.99),
            quality_variance: Some(0.01),
            transfer_latency_p50_ms: Some(1.0),
            transfer_latency_p95_ms: Some(2.0),
            failure_rate: Some(0.001),
            sample_count: Some(100),
            confidence: Some(0.99),
        }
    }

    #[test]
    fn registry_rejects_invalid_measurements_and_duplicate_mappings() {
        let mut inverted = measured_record();
        inverted.transfer_latency_p95_ms = Some(0.5);
        assert!(
            KvCompatibilityRegistry::default()
                .register(inverted)
                .is_err()
        );

        let mut nan = measured_record();
        nan.quality_variance = Some(f64::NAN);
        assert!(KvCompatibilityRegistry::default().register(nan).is_err());

        let mut registry = KvCompatibilityRegistry::default();
        registry.register(measured_record()).unwrap();
        assert!(registry.register(measured_record()).is_err());
    }

    #[test]
    fn invalid_policy_and_source_capability_never_select_transfer() {
        let mut intelligence = KvIntelligence::default();
        intelligence
            .models
            .register(ModelMetadata {
                model: ModelReference::new("source", "1"),
                kv_layout: "layout-1".into(),
                capabilities: ModelCapabilities::normal_prefill(),
            })
            .unwrap();
        intelligence
            .models
            .register(ModelMetadata {
                model: ModelReference::new("target", "1"),
                kv_layout: "layout-1".into(),
                capabilities: ModelCapabilities::normal_prefill()
                    .with_kv(KvStrategy::CrossModelKvTransfer),
            })
            .unwrap();
        intelligence
            .compatibility
            .register(measured_record())
            .unwrap();
        let request = KvPlanningContext {
            source: Some(ModelReference::new("source", "1")),
            target: ModelReference::new("target", "1"),
            requested: KvStrategy::CrossModelKvTransfer,
        };
        assert_eq!(intelligence.select(&request).strategy, KvStrategy::None);
        intelligence.policy.max_failure_rate = f64::NAN;
        assert_eq!(
            intelligence.select(&request).fallback_reason,
            Some(KvFallbackReason::InvalidPolicy)
        );
    }
}
