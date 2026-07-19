use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::Mutex;

/// Identities are attacker-controlled (source addresses, email addresses), so
/// once the map reaches this size, fully-refilled buckets are swept before a
/// new identity is inserted. Only identities still inside their refill window
/// carry state worth keeping, which bounds memory to the actively limited set.
const SWEEP_THRESHOLD_BUCKETS: usize = 10_000;

#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<String, Bucket>>>,
    capacity: f64,
    refill_per_second: f64,
}

struct Bucket {
    tokens: f64,
    updated_at: Instant,
}

impl RateLimiter {
    pub fn new(per_minute: f64, capacity: f64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            capacity,
            refill_per_second: per_minute / 60.0,
        }
    }

    pub async fn check(&self, identity: &str) -> Result<(), u64> {
        let mut buckets = self.inner.lock().await;
        let now = Instant::now();
        if buckets.len() >= SWEEP_THRESHOLD_BUCKETS && !buckets.contains_key(identity) {
            let capacity = self.capacity;
            let refill_per_second = self.refill_per_second;
            buckets.retain(|_, bucket| {
                bucket.tokens
                    + now.duration_since(bucket.updated_at).as_secs_f64() * refill_per_second
                    < capacity
            });
        }
        let bucket = buckets.entry(identity.to_owned()).or_insert(Bucket {
            tokens: self.capacity,
            updated_at: now,
        });
        bucket.tokens = (bucket.tokens
            + now.duration_since(bucket.updated_at).as_secs_f64() * self.refill_per_second)
            .min(self.capacity);
        bucket.updated_at = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - bucket.tokens) / self.refill_per_second)
                .ceil()
                .max(1.0) as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn consumes_a_burst() {
        let limiter = RateLimiter::new(60.0, 2.0);
        assert!(limiter.check("client").await.is_ok());
        assert!(limiter.check("client").await.is_ok());
        assert_eq!(limiter.check("client").await, Err(1));
    }
}
