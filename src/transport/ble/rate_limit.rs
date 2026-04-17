//! BLE send rate limiter and adaptive rate control.
//!
//! Token bucket that throttles BLE sends to match the link's actual throughput.
//! Prevents the L2CAP pipe from filling when mesh-speed data hits a BLE link.
//!
//! The `BleRateAdapter` provides BBR-inspired adaptive rate control using
//! MMP SRTT feedback: reduces rate when RTT rises (congestion), increases
//! when RTT is stable and low.

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

    pub fn rate_bps(&self) -> u64 {
        (self.rate_bytes_per_sec * 8.0) as u64
    }

    pub fn set_rate_bps(&mut self, rate_bps: u64) {
        self.rate_bytes_per_sec = rate_bps as f64 / 8.0;
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

// ============================================================================
// Adaptive Rate Control
// ============================================================================

/// RTT below this (ms) → uncongested → probe for bandwidth.
/// BLE L2CAP baseline RTT is ~150ms; set threshold just above baseline.
const RTT_LOW_MS: f64 = 150.0;

/// RTT above this (ms) → congested → reduce rate to drain queue.
/// Data queuing in L2CAP buffers pushes RTT above baseline quickly.
const RTT_HIGH_MS: f64 = 350.0;

/// Minimum rate. Must be BELOW actual BLE throughput (~34kbps) so the
/// adapter can slow down to match the link. Previous 50kbps was above
/// BLE capacity, causing permanent buffer accumulation.
const MIN_RATE_BPS: u64 = 15_000;

/// BLE 4.2 practical throughput limit (~250 Kbps with L2CAP overhead).
const MAX_RATE_BPS: u64 = 250_000;

/// On congestion: `rate *= MD_FACTOR` (0.7 = 30% reduction).
const MD_FACTOR: f64 = 0.7;

/// On uncongested: `rate += AI_STEP`. Conservative to avoid re-congestion.
const AI_STEP_BPS: u64 = 5_000;

/// BBR-inspired adaptive rate controller for BLE links.
///
/// AIMD using MMP SRTT:
/// - RTT < 300ms: increase rate by 5 Kbps
/// - RTT > 500ms: decrease rate by 30%
/// - 300-500ms: hold steady
pub struct BleRateAdapter {
    current_rate_bps: u64,
    last_update: Instant,
}

impl BleRateAdapter {
    /// Create a new rate adapter starting at the given initial rate.
    pub fn new(initial_rate_bps: u64) -> Self {
        Self {
            current_rate_bps: initial_rate_bps.clamp(MIN_RATE_BPS, MAX_RATE_BPS),
            last_update: Instant::now()
                .checked_sub(Duration::from_secs(10))
                .unwrap_or(Instant::now()),
        }
    }

    pub fn update(&mut self, srtt_ms: f64) -> u64 {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update);
        self.last_update = now;

        if elapsed < Duration::from_secs(1) {
            return self.current_rate_bps;
        }

        if srtt_ms > RTT_HIGH_MS {
            self.current_rate_bps = ((self.current_rate_bps as f64 * MD_FACTOR) as u64)
                .max(MIN_RATE_BPS);
        } else if srtt_ms < RTT_LOW_MS {
            self.current_rate_bps = self
                .current_rate_bps
                .saturating_add(AI_STEP_BPS)
                .min(MAX_RATE_BPS);
        }

        self.current_rate_bps
    }

    /// Get the current recommended rate without updating.
    pub fn current_rate_bps(&self) -> u64 {
        self.current_rate_bps
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

    #[test]
    fn test_rate_adapter_additive_increase() {
        let mut adapter = BleRateAdapter::new(150_000);
        assert_eq!(adapter.current_rate_bps(), 150_000);

        let rate = adapter.update(200.0);
        assert_eq!(rate, 155_000);
    }

    #[test]
    fn test_rate_adapter_multiplicative_decrease() {
        let mut adapter = BleRateAdapter::new(150_000);

        let rate = adapter.update(600.0);
        assert_eq!(rate, 105_000); // 150k * 0.7
    }

    #[test]
    fn test_rate_adapter_steady_zone() {
        let mut adapter = BleRateAdapter::new(150_000);

        let rate = adapter.update(400.0);
        assert_eq!(rate, 150_000);
    }

    #[test]
    fn test_rate_adapter_clamps_to_min() {
        let mut adapter = BleRateAdapter::new(50_000);

        let rate = adapter.update(1000.0);
        assert_eq!(rate, 50_000); // 50k * 0.7 = 35k, clamped to MIN
    }

    #[test]
    fn test_rate_adapter_clamps_to_max() {
        let mut adapter = BleRateAdapter::new(250_000);

        let rate = adapter.update(200.0);
        assert_eq!(rate, 250_000); // already at max
    }

    #[test]
    fn test_rate_setter_getter() {
        let mut limiter = SendRateLimiter::new(100_000, 4000);
        assert_eq!(limiter.rate_bps(), 100_000);
        limiter.set_rate_bps(200_000);
        assert_eq!(limiter.rate_bps(), 200_000);
    }
}
