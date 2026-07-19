use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
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

/// In-process abuse controls for sending authentication email.
///
/// All applicable budgets are checked under one lock and are charged only
/// when the request is accepted. This prevents a request rejected for one
/// reason from exhausting a different recipient's allowance.
#[derive(Clone)]
pub struct EmailSendRateLimiter {
    inner: Arc<Mutex<EmailSendRateState>>,
    limits: EmailSendRateLimits,
}

#[derive(Debug, Clone, Copy)]
pub struct EmailSendRateLimits {
    pub source_window: Duration,
    pub source_budget: u32,
    pub source_daily_window: Duration,
    pub source_daily_budget: u32,
    pub recipient_cooldown: Duration,
    pub recipient_hour_window: Duration,
    pub recipient_hour_limit: usize,
    pub recipient_daily_window: Duration,
    pub recipient_daily_limit: usize,
    pub global_hour_window: Duration,
    pub global_hour_limit: usize,
    pub global_daily_window: Duration,
    pub global_daily_limit: usize,
}

impl Default for EmailSendRateLimits {
    fn default() -> Self {
        Self {
            // One network can send three messages to one recipient, or two
            // messages to two recipients, per rolling fifteen-minute window.
            source_window: Duration::from_secs(15 * 60),
            source_budget: 3,
            source_daily_window: Duration::from_secs(24 * 60 * 60),
            source_daily_budget: 10,
            recipient_cooldown: Duration::from_secs(60),
            recipient_hour_window: Duration::from_secs(60 * 60),
            recipient_hour_limit: 3,
            recipient_daily_window: Duration::from_secs(24 * 60 * 60),
            recipient_daily_limit: 10,
            // This is deliberately a circuit breaker, not the normal
            // admission policy. Deployments with legitimate higher volume
            // should make it configurable before raising it.
            global_hour_window: Duration::from_secs(60 * 60),
            global_hour_limit: 100,
            global_daily_window: Duration::from_secs(24 * 60 * 60),
            global_daily_limit: 1_000,
        }
    }
}

#[derive(Default)]
struct EmailSendRateState {
    sources: HashMap<String, VecDeque<SourceSend>>,
    recipients: HashMap<String, VecDeque<Instant>>,
    global: VecDeque<Instant>,
}

struct SourceSend {
    at: Instant,
    recipient: String,
    window_cost: u32,
    daily_cost: u32,
}

impl EmailSendRateLimiter {
    pub fn new(limits: EmailSendRateLimits) -> Self {
        Self {
            inner: Arc::new(Mutex::new(EmailSendRateState::default())),
            limits,
        }
    }

    pub async fn check(&self, source: &str, recipient: &str) -> Result<(), u64> {
        self.check_at(source, recipient, Instant::now()).await
    }

    async fn check_at(&self, source: &str, recipient: &str, now: Instant) -> Result<(), u64> {
        let mut state = self.inner.lock().await;
        self.sweep(&mut state, source, recipient, now);

        state.sources.entry(source.to_owned()).or_default();
        state.recipients.entry(recipient.to_owned()).or_default();
        let source_events = state
            .sources
            .get(source)
            .expect("source bucket was inserted above");
        let window_cost = recipient_cost(source_events, recipient, now, self.limits.source_window);
        let daily_cost = recipient_cost(
            source_events,
            recipient,
            now,
            self.limits.source_daily_window,
        );
        let recipient_events = state
            .recipients
            .get(recipient)
            .expect("recipient bucket was inserted above");

        let retry_after = [
            weighted_retry_after(
                source_events,
                now,
                self.limits.source_window,
                self.limits.source_budget,
                window_cost,
                |event| event.window_cost,
            ),
            weighted_retry_after(
                source_events,
                now,
                self.limits.source_daily_window,
                self.limits.source_daily_budget,
                daily_cost,
                |event| event.daily_cost,
            ),
            count_retry_after(recipient_events, now, self.limits.recipient_cooldown, 1),
            count_retry_after(
                recipient_events,
                now,
                self.limits.recipient_hour_window,
                self.limits.recipient_hour_limit,
            ),
            count_retry_after(
                recipient_events,
                now,
                self.limits.recipient_daily_window,
                self.limits.recipient_daily_limit,
            ),
            count_retry_after(
                &state.global,
                now,
                self.limits.global_hour_window,
                self.limits.global_hour_limit,
            ),
            count_retry_after(
                &state.global,
                now,
                self.limits.global_daily_window,
                self.limits.global_daily_limit,
            ),
        ]
        .into_iter()
        .flatten()
        .max();

        if let Some(retry_after) = retry_after {
            return Err(retry_after);
        }

        state
            .sources
            .get_mut(source)
            .expect("source bucket was inserted above")
            .push_back(SourceSend {
                at: now,
                recipient: recipient.to_owned(),
                window_cost,
                daily_cost,
            });
        state
            .recipients
            .get_mut(recipient)
            .expect("recipient bucket was inserted above")
            .push_back(now);
        state.global.push_back(now);
        Ok(())
    }

    fn sweep(&self, state: &mut EmailSendRateState, source: &str, recipient: &str, now: Instant) {
        let source_retention = self
            .limits
            .source_window
            .max(self.limits.source_daily_window);
        let recipient_retention = self
            .limits
            .recipient_cooldown
            .max(self.limits.recipient_hour_window)
            .max(self.limits.recipient_daily_window);
        let global_retention = self
            .limits
            .global_hour_window
            .max(self.limits.global_daily_window);

        if let Some(events) = state.sources.get_mut(source) {
            retain_recent_source(events, now, source_retention);
        }
        if let Some(events) = state.recipients.get_mut(recipient) {
            retain_recent(events, now, recipient_retention);
        }
        retain_recent(&mut state.global, now, global_retention);

        if state.sources.len() >= SWEEP_THRESHOLD_BUCKETS && !state.sources.contains_key(source) {
            state.sources.retain(|_, events| {
                retain_recent_source(events, now, source_retention);
                !events.is_empty()
            });
        }
        if state.recipients.len() >= SWEEP_THRESHOLD_BUCKETS
            && !state.recipients.contains_key(recipient)
        {
            state.recipients.retain(|_, events| {
                retain_recent(events, now, recipient_retention);
                !events.is_empty()
            });
        }
    }
}

impl Default for EmailSendRateLimiter {
    fn default() -> Self {
        Self::new(EmailSendRateLimits::default())
    }
}

fn recipient_cost(
    events: &VecDeque<SourceSend>,
    recipient: &str,
    now: Instant,
    window: Duration,
) -> u32 {
    let mut has_recent_send = false;
    for event in events
        .iter()
        .filter(|event| now.duration_since(event.at) < window)
    {
        has_recent_send = true;
        if event.recipient == recipient {
            return 1;
        }
    }
    if has_recent_send { 2 } else { 1 }
}

fn weighted_retry_after(
    events: &VecDeque<SourceSend>,
    now: Instant,
    window: Duration,
    budget: u32,
    incoming_cost: u32,
    cost: impl Fn(&SourceSend) -> u32,
) -> Option<u64> {
    let active = events
        .iter()
        .filter(|event| now.duration_since(event.at) < window)
        .collect::<Vec<_>>();
    let total = active.iter().map(|event| cost(event)).sum::<u32>();
    if total.saturating_add(incoming_cost) <= budget {
        return None;
    }
    let mut released = 0;
    let needed = total.saturating_add(incoming_cost).saturating_sub(budget);
    for event in active {
        released += cost(event);
        if released >= needed {
            return Some(seconds_until_expiry(event.at, now, window));
        }
    }
    Some(window.as_secs().max(1))
}

fn count_retry_after(
    events: &VecDeque<Instant>,
    now: Instant,
    window: Duration,
    limit: usize,
) -> Option<u64> {
    let active = events
        .iter()
        .copied()
        .filter(|event| now.duration_since(*event) < window)
        .collect::<Vec<_>>();
    if active.len() < limit {
        return None;
    }
    let release_index = active.len() + 1 - limit;
    Some(seconds_until_expiry(active[release_index - 1], now, window))
}

fn seconds_until_expiry(event: Instant, now: Instant, window: Duration) -> u64 {
    window
        .saturating_sub(now.duration_since(event))
        .as_secs_f64()
        .ceil()
        .max(1.0) as u64
}

fn retain_recent(events: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while events
        .front()
        .is_some_and(|event| now.duration_since(*event) >= window)
    {
        events.pop_front();
    }
}

fn retain_recent_source(events: &mut VecDeque<SourceSend>, now: Instant, window: Duration) {
    while events
        .front()
        .is_some_and(|event| now.duration_since(event.at) >= window)
    {
        events.pop_front();
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

    fn test_email_limits() -> EmailSendRateLimits {
        EmailSendRateLimits {
            source_window: Duration::from_secs(15 * 60),
            source_budget: 3,
            source_daily_window: Duration::from_secs(24 * 60 * 60),
            source_daily_budget: 10,
            recipient_cooldown: Duration::from_secs(60),
            recipient_hour_window: Duration::from_secs(60 * 60),
            recipient_hour_limit: 3,
            recipient_daily_window: Duration::from_secs(24 * 60 * 60),
            recipient_daily_limit: 10,
            global_hour_window: Duration::from_secs(60 * 60),
            global_hour_limit: 100,
            global_daily_window: Duration::from_secs(24 * 60 * 60),
            global_daily_limit: 1_000,
        }
    }

    #[tokio::test]
    async fn email_sends_have_a_strict_source_window() {
        let limits = EmailSendRateLimits {
            recipient_cooldown: Duration::ZERO,
            recipient_hour_limit: 100,
            recipient_daily_limit: 100,
            ..test_email_limits()
        };
        let limiter = EmailSendRateLimiter::new(limits);
        let start = Instant::now();

        assert!(
            limiter
                .check_at("source", "one@example.com", start)
                .await
                .is_ok()
        );
        assert!(
            limiter
                .check_at("source", "one@example.com", start + Duration::from_secs(1))
                .await
                .is_ok()
        );
        assert!(
            limiter
                .check_at("source", "one@example.com", start + Duration::from_secs(2))
                .await
                .is_ok()
        );
        assert_eq!(
            limiter
                .check_at("source", "one@example.com", start + Duration::from_secs(3))
                .await,
            Err(897)
        );
        assert!(
            limiter
                .check_at(
                    "source",
                    "one@example.com",
                    start + Duration::from_secs(15 * 60)
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_new_recipient_costs_more_to_stop_address_spraying() {
        let limits = EmailSendRateLimits {
            recipient_cooldown: Duration::ZERO,
            ..test_email_limits()
        };
        let limiter = EmailSendRateLimiter::new(limits);
        let start = Instant::now();

        assert!(
            limiter
                .check_at("source", "one@example.com", start)
                .await
                .is_ok()
        );
        assert!(
            limiter
                .check_at("source", "two@example.com", start + Duration::from_secs(1))
                .await
                .is_ok()
        );
        assert!(
            limiter
                .check_at("source", "two@example.com", start + Duration::from_secs(2))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn recipient_limits_apply_across_sources() {
        let limiter = EmailSendRateLimiter::new(test_email_limits());
        let start = Instant::now();

        assert!(
            limiter
                .check_at("source-a", "victim@example.com", start)
                .await
                .is_ok()
        );
        assert_eq!(
            limiter
                .check_at(
                    "source-b",
                    "victim@example.com",
                    start + Duration::from_secs(1)
                )
                .await,
            Err(59)
        );
        assert!(
            limiter
                .check_at(
                    "source-b",
                    "victim@example.com",
                    start + Duration::from_secs(60)
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn rejected_source_does_not_consume_recipient_allowance() {
        let limits = EmailSendRateLimits {
            recipient_cooldown: Duration::ZERO,
            source_budget: 1,
            ..test_email_limits()
        };
        let limiter = EmailSendRateLimiter::new(limits);
        let start = Instant::now();

        assert!(
            limiter
                .check_at("blocked", "first@example.com", start)
                .await
                .is_ok()
        );
        assert!(
            limiter
                .check_at(
                    "blocked",
                    "victim@example.com",
                    start + Duration::from_secs(1)
                )
                .await
                .is_err()
        );
        assert!(
            limiter
                .check_at(
                    "allowed",
                    "victim@example.com",
                    start + Duration::from_secs(2)
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn global_limit_is_a_process_wide_circuit_breaker() {
        let limits = EmailSendRateLimits {
            recipient_cooldown: Duration::ZERO,
            global_hour_limit: 2,
            ..test_email_limits()
        };
        let limiter = EmailSendRateLimiter::new(limits);
        let start = Instant::now();

        assert!(
            limiter
                .check_at("source-a", "one@example.com", start)
                .await
                .is_ok()
        );
        assert!(
            limiter
                .check_at(
                    "source-b",
                    "two@example.com",
                    start + Duration::from_secs(1)
                )
                .await
                .is_ok()
        );
        assert_eq!(
            limiter
                .check_at(
                    "source-c",
                    "three@example.com",
                    start + Duration::from_secs(2)
                )
                .await,
            Err(3_598)
        );
    }
}
