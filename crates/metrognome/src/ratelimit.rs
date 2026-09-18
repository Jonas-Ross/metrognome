//! Token-bucket rate limiting for the iTunes endpoints.
//!
//! Apple documents no number; the Search API is reported to refuse around 20
//! requests per minute per address, and being throttled is slower than being
//! polite. The limiter is not optional and not a knob to turn up.

use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Default request budget. Slightly under the reported ceiling, because the
/// cost of being wrong is a temporary ban and the benefit is a few seconds.
pub const DEFAULT_PER_MINUTE: f64 = 18.0;

/// Default burst size.
///
/// Lets a handful of tracks start immediately rather than spacing out from a
/// standing start; the refill rate still bounds the long-run average.
pub const DEFAULT_BURST: f64 = 5.0;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Bounds the long-run request rate, allowing a small burst.
#[derive(Debug)]
pub struct RateLimiter {
    bucket: Mutex<Bucket>,
    capacity: f64,
    refill_per_sec: f64,
}

impl RateLimiter {
    /// A limiter allowing `per_minute` requests per minute with a burst of
    /// `burst`.
    pub fn new(per_minute: f64, burst: f64) -> Self {
        let capacity = burst.max(1.0);
        RateLimiter {
            bucket: Mutex::new(Bucket {
                tokens: capacity,
                last: Instant::now(),
            }),
            capacity,
            refill_per_sec: (per_minute.max(0.01)) / 60.0,
        }
    }

    /// Wait until a request may be issued, then consume its token.
    ///
    /// The token is taken under the lock and the sleep happens after releasing
    /// it, so concurrent callers queue rather than racing for the same token.
    pub async fn acquire(&self) {
        let wait = {
            let mut bucket = self.bucket.lock().await;
            take(
                &mut bucket,
                Instant::now(),
                self.capacity,
                self.refill_per_sec,
            )
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        RateLimiter::new(DEFAULT_PER_MINUTE, DEFAULT_BURST)
    }
}

/// Consume one token, returning how long the caller must wait first.
///
/// Split from [`RateLimiter::acquire`] so the arithmetic is testable without
/// sleeping. The bucket may go negative and the debt is the wait, which is what
/// spaces a queue of callers evenly.
fn take(bucket: &mut Bucket, now: Instant, capacity: f64, refill_per_sec: f64) -> Duration {
    let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * refill_per_sec).min(capacity);
    bucket.last = now;
    bucket.tokens -= 1.0;
    if bucket.tokens >= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(-bucket.tokens / refill_per_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(tokens: f64, last: Instant) -> Bucket {
        Bucket { tokens, last }
    }

    #[tokio::test]
    async fn burst_is_immediate_then_spacing_kicks_in() {
        let t0 = Instant::now();
        // 60/min == 1/s, burst of 3.
        let (cap, rate) = (3.0, 1.0);
        let mut b = bucket(cap, t0);

        assert_eq!(take(&mut b, t0, cap, rate), Duration::ZERO);
        assert_eq!(take(&mut b, t0, cap, rate), Duration::ZERO);
        assert_eq!(take(&mut b, t0, cap, rate), Duration::ZERO);
        // Burst spent: the fourth caller waits a full refill period.
        let w = take(&mut b, t0, cap, rate);
        assert!((w.as_secs_f64() - 1.0).abs() < 1e-6, "{w:?}");
        // And the fifth waits twice as long, so a queue comes out evenly spaced.
        let w = take(&mut b, t0, cap, rate);
        assert!((w.as_secs_f64() - 2.0).abs() < 1e-6, "{w:?}");
    }

    #[tokio::test]
    async fn tokens_refill_over_time_and_cap_at_capacity() {
        let t0 = Instant::now();
        let (cap, rate) = (5.0, 1.0);
        let mut b = bucket(0.0, t0);

        // Two seconds buys two tokens.
        assert_eq!(
            take(&mut b, t0 + Duration::from_secs(2), cap, rate),
            Duration::ZERO
        );
        assert!((b.tokens - 1.0).abs() < 1e-9);

        // An hour of idling does not bank an hour of requests.
        let mut b = bucket(0.0, t0);
        assert_eq!(
            take(&mut b, t0 + Duration::from_secs(3600), cap, rate),
            Duration::ZERO
        );
        assert!((b.tokens - (cap - 1.0)).abs() < 1e-9, "{}", b.tokens);
    }

    #[tokio::test]
    async fn acquire_does_not_stall_within_the_burst() {
        let limiter = RateLimiter::new(60.0, 3.0);
        let start = std::time::Instant::now();
        for _ in 0..3 {
            limiter.acquire().await;
        }
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
