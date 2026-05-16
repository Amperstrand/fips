//! BLE send rate limiter and adaptive rate control.
//!
//! Two components:
//! 1. `SendRateLimiter`: Token-bucket rate limiter that throttles BLE sends
//!    to match the link's actual throughput. Prevents the L2CAP pipe from
//!    filling when mesh-speed data hits a BLE link.
//! 2. `BleRateAdapter`: BBR-inspired adaptive rate controller using MMP SRTT
//!    feedback. Uses AIMD (Additive Increase / Multiplicative Decrease) to
//!    find the sustainable send rate.
//!
//! Design rationale: BLE L2CAP CoC has limited throughput (~34 kbps observed)
//! and small credit windows. Sending faster than the link can drain causes
//! queue buildup in the L2CAP socket buffer, which manifests as monotonically
//! increasing RTT (see issue #105). The rate adapter uses SRTT as a congestion
//! signal: rising RTT indicates queue buildup → reduce rate; stable low RTT
//! indicates drain → probe for more bandwidth.
//!
//! Reference: The AIMD approach is inspired by TCP Reno/BBR congestion control.
//! TCP uses loss as a congestion signal; we use RTT inflation because BLE
//! L2CAP does not have explicit congestion notification at the link layer.

use std::time::{Duration, Instant};

use tracing::trace;

/// BLE send rate limiter using token bucket algorithm.
///
/// The token bucket is a standard traffic-shaping algorithm (cf. RFC 3290,
/// "An Informal Management Model for Diffserv Router Elements" §2.2).
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

    /// Try to acquire `bytes` tokens without waiting.
    /// Returns `true` if tokens were available and consumed, `false` if not.
    pub fn try_acquire(&mut self, bytes: usize) -> bool {
        if self.rate_bytes_per_sec <= 0.0 {
            return true;
        }
        self.refill();
        if self.tokens >= bytes as f64 {
            self.tokens -= bytes as f64;
            return true;
        }
        false
    }

    /// Acquire `bytes` tokens, waiting if necessary.
    ///
    /// Uses async sleep to wait for tokens, which adds latency to sends when
    /// the rate is throttled. This is intentional — it is better to slow down
    /// at the sender than to fill the L2CAP buffer, which causes RTT inflation
    /// and eventual link instability.
    pub async fn acquire(&mut self, bytes: usize) {
        if self.rate_bytes_per_sec <= 0.0 {
            return;
        }

        let mut waits = 0u32;
        loop {
            self.refill();

            if self.tokens >= bytes as f64 {
                self.tokens -= bytes as f64;
                if waits > 0 || bytes > 512 {
                    trace!(
                        bytes,
                        tokens_remaining = self.tokens as u32,
                        waits,
                        rate_kbps = (self.rate_bytes_per_sec * 8.0 / 1000.0) as u32,
                        burst_cap = self.burst_bytes as u32,
                        "rate_limiter: acquire completed"
                    );
                }
                return;
            }

            let deficit = bytes as f64 - self.tokens;
            let wait_secs = deficit / self.rate_bytes_per_sec;
            let wait = Duration::from_secs_f64(wait_secs).max(Duration::from_millis(1));
            waits += 1;
            if waits == 1 {
                trace!(
                    bytes,
                    tokens = self.tokens as u32,
                    deficit = deficit as u32,
                    wait_ms = wait.as_millis() as u32,
                    rate_kbps = (self.rate_bytes_per_sec * 8.0 / 1000.0) as u32,
                    "rate_limiter: waiting for tokens"
                );
            }
            tokio::time::sleep(wait).await;
        }
    }

    pub fn rate_bps(&self) -> u64 {
        (self.rate_bytes_per_sec * 8.0) as u64
    }

    pub fn set_rate_bps(&mut self, rate_bps: u64) {
        self.rate_bytes_per_sec = rate_bps as f64 / 8.0;
    }

    /// Standard token bucket refill: `tokens += elapsed × rate`.
    /// Caps at `burst_bytes` (bucket capacity) to prevent infinite accumulation.
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
/// BLE baseline (heartbeat) RTT ~100ms; data-transfer RTT ~250-400ms.
/// Set at 200ms to allow probing when link is lightly loaded.
const RTT_LOW_MS: f64 = 200.0;

/// RTT above this (ms) → congested → reduce rate to drain queue.
/// Lowered from 400ms to 300ms: experiments show RTT grows ~167ms/s, so
/// catching it at 300ms gives us ~2s of headroom before the queue becomes
/// critical. At 300ms the queue is building but still drainable.
const RTT_HIGH_MS: f64 = 300.0;

/// RTT above this (ms) → severe congestion → apply stronger backoff.
/// Lowered from 1000ms to 600ms: by 600ms the RTT has already been growing
/// for ~3-4 seconds. The 50% rate cut is aggressive to drain quickly before
/// the queue reaches the point of causing multi-second RTT.
const RTT_SEVERE_MS: f64 = 600.0;

/// Minimum rate. Must be BELOW actual BLE throughput (~34kbps) so the
/// adapter can slow down to match the link. Previous 50kbps was above
/// BLE capacity, causing permanent buffer accumulation.
const MIN_RATE_BPS: u64 = 10_000;

/// Maximum sustainable BLE rate. Experiments show the L2CAP link dies after
/// 3-5 minutes at ~110 kbps. Lowered from 50kbps to 35kbps — below observed
/// BLE throughput (~34kbps) to prevent the AIMD probe from ever exceeding
/// the link's actual capacity. The adapter probes UP from MIN but must not
/// overshoot into queue-accumulation territory.
pub const MAX_RATE_BPS: u64 = 35_000;

/// On congestion: `rate *= MD_FACTOR` (0.6 = 40% reduction).
/// Lowered from 0.7 to 0.6 for more aggressive drain.
const MD_FACTOR: f64 = 0.6;

/// On severe congestion: `rate *= MD_SEVERE_FACTOR` (0.4 = 60% reduction).
/// Lowered from 0.5 to 0.4 for more aggressive emergency drain.
const MD_SEVERE_FACTOR: f64 = 0.4;

/// On uncongested: `rate += AI_STEP`. Conservative to avoid re-congestion.
/// Lowered from 5000 to 2000 for slower probing to stay below BLE capacity.
const AI_STEP_BPS: u64 = 2_000;

/// BBR-inspired adaptive rate controller for BLE links.
///
/// AIMD using MMP SRTT:
/// - RTT > 1000ms: severe congestion → decrease rate by 50%
/// - RTT > 400ms: congestion → decrease rate by 30%
/// - RTT < 200ms: uncongested → increase rate by 5 Kbps
/// - 200-400ms: hold steady
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

    /// Update the rate based on the latest SRTT measurement.
    ///
    /// AIMD rate control using SRTT as congestion signal:
    ///
    /// SRTT > 1000ms: Severe congestion → rate *= 0.5 (50% cut)
    ///   Aggressive drain. Queue is critically full.
    /// SRTT > 400ms:  Congestion → rate *= 0.7 (30% cut)
    ///   Moderate drain. Queue is building but not critical.
    /// SRTT < 200ms:  Uncongested → rate += 5 Kbps (additive increase)
    ///   Probe for more bandwidth. Conservative to avoid re-congestion.
    /// 200-400ms:     Hold steady
    ///   Link is operating in its normal range.
    ///
    /// This is a simplified BBR-like approach. Unlike TCP BBR which models
    /// bandwidth and RTT separately, we use a single SRTT signal because
    /// BLE links have a single bottleneck (the L2CAP channel).
    ///
    /// Minimum update interval: 1 second. Prevents over-reaction to
    /// transient RTT spikes from individual samples.
    pub fn update(&mut self, srtt_ms: f64) -> u64 {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update);
        self.last_update = now;

        if elapsed < Duration::from_secs(1) {
            return self.current_rate_bps;
        }

        let old_rate = self.current_rate_bps;

        let decision = if srtt_ms > RTT_SEVERE_MS {
            self.current_rate_bps =
                ((self.current_rate_bps as f64 * MD_SEVERE_FACTOR) as u64).max(MIN_RATE_BPS);
            "decrease_severe"
        } else if srtt_ms > RTT_HIGH_MS {
            self.current_rate_bps =
                ((self.current_rate_bps as f64 * MD_FACTOR) as u64).max(MIN_RATE_BPS);
            "decrease"
        } else if srtt_ms < RTT_LOW_MS {
            self.current_rate_bps = self
                .current_rate_bps
                .saturating_add(AI_STEP_BPS)
                .min(MAX_RATE_BPS);
            "increase"
        } else {
            "hold"
        };

        if old_rate != self.current_rate_bps {
            // Log rate changes to the BLE event log for experiment correlation.
            // Each entry includes the SRTT sample that triggered the decision,
            // the old/new rates, and the decision name for filtering.
            super::event_log::log(
                "rate_adapter",
                "",
                &[
                    ("srtt_ms", &format!("{:.1}", srtt_ms)),
                    ("old_rate_bps", &old_rate.to_string()),
                    ("new_rate_bps", &self.current_rate_bps.to_string()),
                    ("decision", decision),
                ],
            );
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
        let mut adapter = BleRateAdapter::new(20_000);
        assert_eq!(adapter.current_rate_bps(), 20_000);

        // SRTT < RTT_LOW → +AI_STEP_BPS (2000)
        let rate = adapter.update(150.0);
        assert_eq!(rate, 22_000);
    }

    #[test]
    fn test_rate_adapter_multiplicative_decrease() {
        // SRTT > RTT_HIGH (300ms), < RTT_SEVERE (600ms) → rate × MD_FACTOR (0.6)
        let mut adapter = BleRateAdapter::new(30_000);
        let rate = adapter.update(350.0);
        assert_eq!(rate, 18_000); // 30k × 0.6
    }

    #[test]
    fn test_rate_adapter_steady_zone() {
        // SRTT in [RTT_LOW, RTT_HIGH] → hold
        let mut adapter = BleRateAdapter::new(25_000);
        let rate = adapter.update(250.0);
        assert_eq!(rate, 25_000);
    }

    #[test]
    fn test_rate_adapter_clamps_to_min() {
        // SRTT > RTT_SEVERE (600ms) → rate × MD_SEVERE (0.4), clamped to MIN_RATE_BPS
        // 15k × 0.4 = 6000 < MIN_RATE_BPS (10k)
        let mut adapter = BleRateAdapter::new(15_000);
        let rate = adapter.update(800.0);
        assert_eq!(rate, 10_000);
    }

    #[test]
    fn test_rate_adapter_clamps_to_max() {
        let mut adapter = BleRateAdapter::new(50_000);
        assert_eq!(adapter.current_rate_bps(), 35_000); // constructor clamps to MAX_RATE_BPS

        // SRTT < RTT_LOW → +AI_STEP_BPS, but already at MAX → stays at MAX
        let rate = adapter.update(100.0);
        assert_eq!(rate, 35_000);
    }

    #[test]
    fn test_rate_setter_getter() {
        let mut limiter = SendRateLimiter::new(100_000, 4000);
        assert_eq!(limiter.rate_bps(), 100_000);
        limiter.set_rate_bps(200_000);
        assert_eq!(limiter.rate_bps(), 200_000);
    }
}
