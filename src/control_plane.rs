//! Immutable, versioned control-plane state used to make planning reproducible.
use crate::kv::{KvIntelligence, KvPolicy};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StateVersion(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlPlaneVersions {
    pub graph: StateVersion,
    pub health: StateVersion,
    pub models: StateVersion,
    pub kv: StateVersion,
    pub policy: StateVersion,
    pub capacity: StateVersion,
    pub planner: StateVersion,
}
impl Default for ControlPlaneVersions {
    fn default() -> Self {
        Self {
            graph: StateVersion(0),
            health: StateVersion(0),
            models: StateVersion(0),
            kv: StateVersion(0),
            policy: StateVersion(0),
            capacity: StateVersion(0),
            planner: StateVersion(1),
        }
    }
}
#[derive(Debug, Clone)]
pub struct ControlPlaneSnapshot {
    pub versions: ControlPlaneVersions,
    pub kv: KvIntelligence,
}
impl ControlPlaneSnapshot {
    pub fn new(kv: KvIntelligence, mut versions: ControlPlaneVersions) -> Self {
        versions.models = StateVersion(kv.models.revision());
        versions.kv = StateVersion(kv.compatibility.revision());
        Self { versions, kv }
    }
    pub fn is_current_for(&self, other: &Self) -> bool {
        self.versions == other.versions
    }
    pub fn policy(&self) -> KvPolicy {
        self.kv.policy
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_revisions_are_captured_and_changes_are_stale() {
        let a = ControlPlaneSnapshot::new(KvIntelligence::default(), Default::default());
        let mut b = a.clone();
        b.versions.health = StateVersion(1);
        assert!(!a.is_current_for(&b));
        assert_eq!(a.versions.planner, StateVersion(1));
    }
}
