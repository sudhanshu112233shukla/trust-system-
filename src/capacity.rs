//! Deterministic, simulated compute-capacity snapshots for planning feasibility.
use crate::control_plane::StateVersion;
use crate::kv::ModelReference;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq)]
pub struct ComputeNode {
    pub id: String,
    pub region: String,
    pub accelerator: String,
    pub total_memory_mb: u64,
    pub available_memory_mb: u64,
    pub utilization: f64,
    pub active_requests: u32,
    pub queue_depth: u32,
    pub healthy: bool,
    pub resident_models: BTreeSet<ModelReference>,
}
impl ComputeNode {
    fn validate(&self) -> Result<(), CapacityError> {
        if !id(&self.id)
            || !id(&self.region)
            || !id(&self.accelerator)
            || self.total_memory_mb == 0
            || self.available_memory_mb > self.total_memory_mb
            || !self.utilization.is_finite()
            || !(0.0..=1.0).contains(&self.utilization)
        {
            Err(CapacityError::InvalidNode(self.id.clone()))
        } else {
            Ok(())
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityRequirement {
    pub model: ModelReference,
    pub region: Option<String>,
    pub min_available_memory_mb: u64,
    pub max_queue_depth: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapacityRejection {
    Unhealthy,
    RegionMismatch,
    ModelNotResident,
    InsufficientMemory,
    QueueLimit,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapacityError {
    InvalidNode(String),
    DuplicateNode(String),
}
impl std::fmt::Display for CapacityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CapacityError {}
#[derive(Debug, Clone)]
pub struct CapacitySnapshot {
    nodes: BTreeMap<String, ComputeNode>,
    revision: StateVersion,
}
impl Default for CapacitySnapshot {
    fn default() -> Self {
        Self {
            nodes: BTreeMap::new(),
            revision: StateVersion(0),
        }
    }
}
impl CapacitySnapshot {
    pub fn register(&mut self, node: ComputeNode) -> Result<(), CapacityError> {
        node.validate()?;
        if self.nodes.contains_key(&node.id) {
            return Err(CapacityError::DuplicateNode(node.id));
        }
        self.nodes.insert(node.id.clone(), node);
        self.revision.0 = self.revision.0.saturating_add(1);
        Ok(())
    }
    pub fn revision(&self) -> StateVersion {
        self.revision
    }
    pub fn evaluate(
        &self,
        requirement: &CapacityRequirement,
    ) -> (Vec<&ComputeNode>, Vec<(String, CapacityRejection)>) {
        let mut eligible = Vec::new();
        let mut rejected = Vec::new();
        for (id, node) in &self.nodes {
            let reason = if !node.healthy {
                Some(CapacityRejection::Unhealthy)
            } else if requirement
                .region
                .as_ref()
                .is_some_and(|r| r != &node.region)
            {
                Some(CapacityRejection::RegionMismatch)
            } else if !node.resident_models.contains(&requirement.model) {
                Some(CapacityRejection::ModelNotResident)
            } else if node.available_memory_mb < requirement.min_available_memory_mb {
                Some(CapacityRejection::InsufficientMemory)
            } else if node.queue_depth > requirement.max_queue_depth {
                Some(CapacityRejection::QueueLimit)
            } else {
                None
            };
            if let Some(reason) = reason {
                rejected.push((id.clone(), reason));
            } else {
                eligible.push(node);
            }
        }
        (eligible, rejected)
    }
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn node(id: &str) -> ComputeNode {
        ComputeNode {
            id: id.into(),
            region: "region-a".into(),
            accelerator: "simulated".into(),
            total_memory_mb: 100,
            available_memory_mb: 50,
            utilization: 0.5,
            active_requests: 0,
            queue_depth: 0,
            healthy: true,
            resident_models: BTreeSet::from([ModelReference::new("model", "1")]),
        }
    }
    #[test]
    fn capacity_filter_is_ordered_and_never_selects_infeasible_nodes() {
        let mut snapshot = CapacitySnapshot::default();
        snapshot.register(node("b")).unwrap();
        let mut bad = node("a");
        bad.healthy = false;
        snapshot.register(bad).unwrap();
        let (e, r) = snapshot.evaluate(&CapacityRequirement {
            model: ModelReference::new("model", "1"),
            region: Some("region-a".into()),
            min_available_memory_mb: 10,
            max_queue_depth: 0,
        });
        assert_eq!(e[0].id, "b");
        assert_eq!(r, vec![("a".into(), CapacityRejection::Unhealthy)]);
    }
}
