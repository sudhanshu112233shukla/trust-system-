use std::time::{Duration, Instant};

use crate::sidecar_config::RateLimitConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitExceeded {
    pub retry_after: Duration,
}

#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_second: f64,
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            capacity: config.capacity as f64,
            refill_per_second: config.refill_per_second as f64,
            tokens: config.capacity as f64,
            last_refill: Instant::now(),
        }
    }

    pub fn try_acquire(&mut self) -> Result<(), RateLimitExceeded> {
        self.try_acquire_at(Instant::now())
    }

    pub fn try_acquire_at(&mut self, now: Instant) -> Result<(), RateLimitExceeded> {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }

        let missing_tokens = 1.0 - self.tokens;
        let wait_seconds = missing_tokens / self.refill_per_second;
        Err(RateLimitExceeded {
            retry_after: Duration::from_secs_f64(wait_seconds.max(0.001)),
        })
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_second).min(self.capacity);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_enforces_capacity_then_refills() {
        let config = RateLimitConfig {
            capacity: 2,
            refill_per_second: 4,
        };
        let mut bucket = TokenBucket::new(&config);
        let now = Instant::now();
        bucket.last_refill = now;

        assert!(bucket.try_acquire_at(now).is_ok());
        assert!(bucket.try_acquire_at(now).is_ok());
        assert!(bucket.try_acquire_at(now).is_err());
        assert!(
            bucket
                .try_acquire_at(now + Duration::from_millis(250))
                .is_ok()
        );
    }
}
