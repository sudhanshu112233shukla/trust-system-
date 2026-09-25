//! Bounded local admission control for planning and execution work.
//!
//! The controller limits both process-wide and per-tenant concurrent work.
//! It intentionally has no distributed coordination: a multi-instance service
//! needs a shared coordinator if it requires cluster-wide fairness.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const MAX_IDENTIFIER_LENGTH: usize = 128;

/// Limits applied before a request consumes local planning capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionPolicy {
    pub global_in_flight: usize,
    pub per_tenant_in_flight: usize,
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            global_in_flight: 1_024,
            per_tenant_in_flight: 64,
        }
    }
}

impl AdmissionPolicy {
    /// Reject impossible limits at startup instead of silently weakening isolation.
    pub fn validate(self) -> Result<(), AdmissionError> {
        if self.global_in_flight == 0 {
            return Err(AdmissionError::InvalidPolicy(
                "global_in_flight must be greater than zero",
            ));
        }
        if self.per_tenant_in_flight == 0 {
            return Err(AdmissionError::InvalidPolicy(
                "per_tenant_in_flight must be greater than zero",
            ));
        }
        if self.per_tenant_in_flight > self.global_in_flight {
            return Err(AdmissionError::InvalidPolicy(
                "per_tenant_in_flight cannot exceed global_in_flight",
            ));
        }
        Ok(())
    }
}

/// A stable admission failure that callers can map to a bounded-overload response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    InvalidPolicy(&'static str),
    InvalidTenant,
    GlobalLimit { limit: usize },
    TenantLimit { tenant: String, limit: usize },
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPolicy(message) => {
                write!(formatter, "invalid admission policy: {message}")
            }
            Self::InvalidTenant => write!(formatter, "tenant identifier is invalid"),
            Self::GlobalLimit { limit } => {
                write!(
                    formatter,
                    "local admission limit of {limit} concurrent requests reached"
                )
            }
            Self::TenantLimit { tenant, limit } => write!(
                formatter,
                "tenant {tenant} reached its local admission limit of {limit} concurrent requests"
            ),
        }
    }
}

impl std::error::Error for AdmissionError {}

#[derive(Debug, Default)]
struct AdmissionState {
    total_in_flight: usize,
    tenant_in_flight: BTreeMap<String, usize>,
}

/// A bounded snapshot suitable for local metrics. It contains only active tenants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionSnapshot {
    pub total_in_flight: usize,
    pub tenant_in_flight: BTreeMap<String, usize>,
}

/// Thread-safe, process-local admission controller.
#[derive(Debug, Clone)]
pub struct AdmissionController {
    policy: AdmissionPolicy,
    state: Arc<Mutex<AdmissionState>>,
}

impl AdmissionController {
    /// Creates an empty controller after validating all limits.
    pub fn new(policy: AdmissionPolicy) -> Result<Self, AdmissionError> {
        policy.validate()?;
        Ok(Self {
            policy,
            state: Arc::new(Mutex::new(AdmissionState::default())),
        })
    }

    /// Atomically admits one request or returns a deterministic overload error.
    ///
    /// The returned permit releases its slot on drop, including unwinding paths,
    /// so callers cannot accidentally retain capacity after a failed request.
    pub fn admit(&self, tenant: &str) -> Result<AdmissionPermit, AdmissionError> {
        if !valid_identifier(tenant) {
            return Err(AdmissionError::InvalidTenant);
        }

        let mut state = lock(&self.state);
        if state.total_in_flight >= self.policy.global_in_flight {
            return Err(AdmissionError::GlobalLimit {
                limit: self.policy.global_in_flight,
            });
        }

        let tenant_count = state
            .tenant_in_flight
            .get(tenant)
            .copied()
            .unwrap_or_default();
        if tenant_count >= self.policy.per_tenant_in_flight {
            return Err(AdmissionError::TenantLimit {
                tenant: tenant.to_owned(),
                limit: self.policy.per_tenant_in_flight,
            });
        }

        state.total_in_flight += 1;
        state
            .tenant_in_flight
            .insert(tenant.to_owned(), tenant_count + 1);
        Ok(AdmissionPermit {
            tenant: tenant.to_owned(),
            state: Arc::clone(&self.state),
        })
    }

    /// Returns a point-in-time view whose tenant cardinality is bounded by the global limit.
    pub fn snapshot(&self) -> AdmissionSnapshot {
        let state = lock(&self.state);
        AdmissionSnapshot {
            total_in_flight: state.total_in_flight,
            tenant_in_flight: state.tenant_in_flight.clone(),
        }
    }

    /// Returns the immutable policy used by this controller.
    pub fn policy(&self) -> AdmissionPolicy {
        self.policy
    }
}

/// An active admission slot. Dropping it always releases exactly one slot.
#[derive(Debug)]
pub struct AdmissionPermit {
    tenant: String,
    state: Arc<Mutex<AdmissionState>>,
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        if let Some(count) = state.tenant_in_flight.get_mut(&self.tenant) {
            *count -= 1;
            if *count == 0 {
                state.tenant_in_flight.remove(&self.tenant);
            }
            state.total_in_flight -= 1;
        }
    }
}

fn lock(state: &Mutex<AdmissionState>) -> std::sync::MutexGuard<'_, AdmissionState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn policy_rejects_zero_and_incoherent_limits() {
        for policy in [
            AdmissionPolicy {
                global_in_flight: 0,
                per_tenant_in_flight: 1,
            },
            AdmissionPolicy {
                global_in_flight: 1,
                per_tenant_in_flight: 0,
            },
            AdmissionPolicy {
                global_in_flight: 1,
                per_tenant_in_flight: 2,
            },
        ] {
            assert!(AdmissionController::new(policy).is_err());
        }
    }

    #[test]
    fn controller_enforces_global_and_tenant_limits_then_releases_on_drop() {
        let controller = AdmissionController::new(AdmissionPolicy {
            global_in_flight: 3,
            per_tenant_in_flight: 2,
        })
        .unwrap();
        let first = controller.admit("acme").unwrap();
        let second = controller.admit("acme").unwrap();
        assert!(matches!(
            controller.admit("acme"),
            Err(AdmissionError::TenantLimit { tenant, limit }) if tenant == "acme" && limit == 2
        ));
        let other = controller.admit("beta").unwrap();
        assert!(matches!(
            controller.admit("gamma"),
            Err(AdmissionError::GlobalLimit { limit: 3 })
        ));

        drop(first);
        let third = controller.admit("gamma").unwrap();
        assert_eq!(controller.snapshot().total_in_flight, 3);
        drop((second, other, third));
        assert_eq!(
            controller.snapshot(),
            AdmissionSnapshot {
                total_in_flight: 0,
                tenant_in_flight: BTreeMap::new(),
            }
        );
    }

    #[test]
    fn invalid_tenants_are_rejected_without_consuming_capacity() {
        let controller = AdmissionController::new(AdmissionPolicy::default()).unwrap();
        for tenant in ["", "bad\nname", "tenant with space", "t\u{00e9}nant"] {
            assert!(matches!(
                controller.admit(tenant),
                Err(AdmissionError::InvalidTenant)
            ));
        }
        assert!(matches!(
            controller.admit(&"a".repeat(129)),
            Err(AdmissionError::InvalidTenant)
        ));
        assert_eq!(controller.snapshot().total_in_flight, 0);
    }

    #[test]
    fn concurrent_admission_never_exceeds_the_configured_bound() {
        let controller = AdmissionController::new(AdmissionPolicy {
            global_in_flight: 8,
            per_tenant_in_flight: 4,
        })
        .unwrap();
        let mut workers = Vec::new();
        for worker_id in 0..16 {
            let controller = controller.clone();
            workers.push(thread::spawn(move || {
                let tenant = format!("tenant-{}", worker_id % 2);
                for _ in 0..100 {
                    if let Ok(permit) = controller.admit(&tenant) {
                        assert!(controller.snapshot().total_in_flight <= 8);
                        drop(permit);
                    }
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(controller.snapshot().total_in_flight, 0);
    }
}
