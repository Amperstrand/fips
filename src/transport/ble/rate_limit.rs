//! BLE send rate limiter.
//!
//! Token bucket that throttles BLE sends to match the link's actual throughput.
//! Prevents the L2CAP pipe from filling when mesh-speed data hits a BLE link.

use std::time::{Duration, Instant};

/// BLE send rate limiter using token bucket algorithm.
///
/// Tokens represent bytes. The bucket refills at `rate_bytes_per_sec` and
/// caps at `burst_bytes`. Before each send, `acquire(bytes)` waits until
/// enough tokens are available.
pub struct SendRateLimiter {
    rate_bytes_per_sec: f64,
    burst_bytes: f64,
    tokens: f64,
    last_refill: Instant,
}

impl SendRateLimiter {
    /// Create a new rate limiter.
    ///
    /// `rate_bps` is in bits per second (divided by 8 for bytes).
    /// `burst_bytes` is the maximum burst size (bucket capacity).
    pub fn new(rate_bps: u64, burst_bytes: u32) -> Self {
        let rate_bytes_per_sec = rate_bps as f64 / 8.0;
        Self {
            rate_bytes_per_sec,
            burst_bytes: burst_bytes as f64,
            tokens: burst_bytes as f64,
            last_refill: Instant::now(),
        }
    }

    /// Acquire `bytes` tokens, waiting if necessary.
    pub async fn acquire(&mut self, bytes: usize) {
        if self.rate_bytes_per_sec <= 0.0 {
            return;
        }

        loop {
            self.refill();

            if self.tokens >= bytes as f64 {
                self.tokens -= bytes as f64;
                return;
            }

            let deficit = bytes as f64 - self.tokens;
            let wait_secs = deficit / self.rate_bytes_per_sec;
            let wait = Duration::from_secs_f64(wait_secs).max(Duration::from_millis(1));
            tokio::time::sleep(wait).await;
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens += elapsed * self.rate_bytes_per_sec;
        if self.tokens > self.burst_bytes {
            self.tokens = self.burst_bytes;
        }
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_burst_allows_immediate_send() {
        let mut limiter = SendRateLimiter::new(8000, 100);
        let now = Instant::now();

        limiter.acquire(50).await;
        limiter.acquire(50).await;

        assert!(now.elapsed() < Duration::from_millis(10));
    }

    #[tokio::test]
    async fn test_rate_limited_send_waits() {
        let mut limiter = SendRateLimiter::new(8000, 100);

        limiter.acquire(100).await;

        let before = Instant::now();
        limiter.acquire(100).await;
        let elapsed = before.elapsed();

        assert!(elapsed >= Duration::from_millis(90));
    }

    #[tokio::test]
    async fn test_zero_rate_is_unlimited() {
        let mut limiter = SendRateLimiter::new(0, 100);

        let now = Instant::now();
        limiter.acquire(10000).await;
        assert!(now.elapsed() < Duration::from_millis(10));
    }
}
