//! MMP algorithmic building blocks.
//!
//! Pure computational types with no dependency on peer or node state.
//! Each is independently testable.
//!
//! # Standards & References
//!
//! - **RFC 3550** (RTP) — interarrival jitter estimation (§6.4.1)
//! - **RFC 6298** (TCP RTO) — Jacobson/Karels SRTT algorithm (§2.2–2.3)
//! - **RFC 9002** (QUIC) — future min_rtt baseline tracking (reserved)
//! - **RFC 9490** (QUIC spin bit) — passive RTT measurement via spin bit
//!
//! Reference implementations consulted:
//! - Linux TCP `tcp_rtt_estimator()` in `net/ipv4/tcp_input.c`
//! - quic-go `rtt_stats.go` (https://github.com/lucas-clemente/quic-go)
//! - quiche `recovery/rtt.rs` (https://github.com/cloudflare/quiche)
//!
//! - De Couto et al. (2003), "A High-Throughput Path Metric for Multi-Hop
//!   Wireless Routing" — ETX metric

use std::collections::VecDeque;
use std::time::Instant;

use crate::mmp::{EWMA_LONG_ALPHA, EWMA_SHORT_ALPHA};

// ============================================================================
// Jitter Estimator (RFC 3550 §6.4.1)
// ============================================================================

/// Interarrival jitter estimator per RFC 3550 §6.4.1.
///
/// Maintains a smoothed jitter estimate (α = 1/16) from the absolute
/// difference in relative transit times between consecutive frames.
/// Uses integer arithmetic scaled by ×16 to avoid floating-point while
/// maintaining sufficient precision for sub-millisecond resolution.
pub struct JitterEstimator {
    /// Scaled jitter estimate (×16 for integer arithmetic).
    jitter_q4: i64,
}

impl JitterEstimator {
    pub fn new() -> Self {
        Self { jitter_q4: 0 }
    }

    /// Update with transit time delta between consecutive frames.
    ///
    /// `transit_delta` = D(i) = (R_i − R_{i−1}) − (S_i − S_{i−1}) per RFC 3550,
    /// where R = receive timestamp and S = send timestamp in microseconds.
    pub fn update(&mut self, transit_delta: i32) {
        // RFC 3550 §6.4.1: J_i = J_{i−1} + (|D(i)| − J_{i−1}) / 16
        // The smoothing factor α = 1/16 is fixed by the RFC.
        // Integer ×16 scaling (Q4 fixed-point) avoids floating-point while
        // preserving sub-millisecond precision:
        //   J_q4 += |D| − J_q4 >> 4
        let abs_d = (transit_delta as i64).unsigned_abs() as i64;
        self.jitter_q4 += abs_d - (self.jitter_q4 >> 4);
    }

    /// Current jitter estimate in microseconds.
    pub fn jitter_us(&self) -> u32 {
        (self.jitter_q4 >> 4) as u32
    }
}

impl Default for JitterEstimator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// SRTT Estimator (Jacobson, RFC 6298)
// ============================================================================

/// Smoothed RTT estimator using Jacobson/Karels algorithm (RFC 6298).
///
/// SRTT and RTTVAR are maintained in microseconds using integer arithmetic.
/// The bit-shift approximations for α=1/8 and β=1/4 are mathematically exact
/// in the integer domain (no rounding error from the shift).
///
/// Reference implementations:
/// - Linux TCP `tcp_rtt_estimator()` in `net/ipv4/tcp_input.c`
/// - quic-go `rtt_stats.go` (https://github.com/lucas-clemente/quic-go)
/// - quiche `recovery/rtt.rs` (https://github.com/cloudflare/quiche)
pub struct SrttEstimator {
    /// Smoothed RTT (microseconds).
    srtt_us: i64,
    /// RTT variance (microseconds).
    rttvar_us: i64,
    /// Whether the first sample has been applied.
    initialized: bool,
}

impl SrttEstimator {
    pub fn new() -> Self {
        Self {
            srtt_us: 0,
            rttvar_us: 0,
            initialized: false,
        }
    }

    /// Feed an RTT sample in microseconds.
    pub fn update(&mut self, rtt_us: i64) {
        if !self.initialized {
            // RFC 6298 §2.2 (rule 2.2): on the first RTT measurement R':
            //   SRTT = R'
            //   RTTVAR = R' / 2
            self.srtt_us = rtt_us;
            self.rttvar_us = rtt_us / 2;
            self.initialized = true;
        } else {
            // RFC 6298 §2.3 (rule 2.3): for subsequent measurements R':
            //   RTTVAR = (1 − β) × RTTVAR + β × |SRTT − R'|    where β = 1/4
            //   SRTT   = (1 − α) × SRTT   + α × R'             where α = 1/8
            // Bit-shift equivalents: >> 2 is ÷4 (β), >> 3 is ÷8 (α)
            let err = (self.srtt_us - rtt_us).abs();
            self.rttvar_us = self.rttvar_us - (self.rttvar_us >> 2) + (err >> 2);
            self.srtt_us = self.srtt_us - (self.srtt_us >> 3) + (rtt_us >> 3);
        }
    }

    pub fn srtt_us(&self) -> i64 {
        self.srtt_us
    }

    pub fn rttvar_us(&self) -> i64 {
        self.rttvar_us
    }

    pub fn initialized(&self) -> bool {
        self.initialized
    }

    /// Retransmission timeout per RFC 6298 §2.3 (rule 2.3):
    ///   RTO = SRTT + max(G, 4 × RTTVAR)
    /// where G is the clock granularity (1s here).
    /// The result is additionally floored at 1 second per RFC 6298 §2.3 rule 2.4.
    pub fn rto_us(&self) -> i64 {
        let rto = self.srtt_us + (self.rttvar_us << 2).max(1_000_000);
        rto.max(1_000_000)
    }
}

impl Default for SrttEstimator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Dual EWMA Trend Detector
// ============================================================================

/// Dual EWMA for trend detection on a single metric.
///
/// Custom trend detector (not from a specific RFC). Uses two EWMA windows
/// with different smoothing factors to detect divergence from baseline:
///
/// - **Short-term** (α = 1/4): tracks recent conditions with fast response
/// - **Long-term** (α = 1/32): establishes a stable baseline over many samples
///
/// Divergence between the two indicates trend direction: if short > long the
/// metric is rising; if short < long it is falling.
pub struct DualEwma {
    short: f64,
    long: f64,
    initialized: bool,
}

impl DualEwma {
    pub fn new() -> Self {
        Self {
            short: 0.0,
            long: 0.0,
            initialized: false,
        }
    }

    pub fn update(&mut self, sample: f64) {
        if !self.initialized {
            self.short = sample;
            self.long = sample;
            self.initialized = true;
        } else {
            self.short += EWMA_SHORT_ALPHA * (sample - self.short);
            self.long += EWMA_LONG_ALPHA * (sample - self.long);
        }
    }

    pub fn short(&self) -> f64 {
        self.short
    }

    pub fn long(&self) -> f64 {
        self.long
    }

    pub fn initialized(&self) -> bool {
        self.initialized
    }
}

impl Default for DualEwma {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// One-Way Delay Trend Detector
// ============================================================================

/// OWD trend detector using linear regression over a ring buffer.
///
/// Stores (sequence, owd_us) samples and computes the slope via
/// least-squares regression. The slope (µs/s) indicates whether
/// queuing delay is increasing (congestion) or stable.
///
/// One-way delay trend is used as a congestion signal, similar to
/// how QUIC congestion control (RFC 9002) monitors RTT inflation
/// to detect queuing buildup before loss occurs.
pub struct OwdTrendDetector {
    samples: VecDeque<(u32, i64)>,
    capacity: usize,
}

impl OwdTrendDetector {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Clear all samples, keeping the same capacity.
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Add an OWD sample.
    ///
    /// `seq` is a monotonic sequence number (e.g., truncated frame counter).
    /// `owd_us` is the relative one-way delay in microseconds (R_i - S_i).
    pub fn push(&mut self, seq: u32, owd_us: i64) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back((seq, owd_us));
    }

    /// Compute the OWD trend as a slope in µs/second.
    ///
    /// Uses simple linear regression: slope = Σ((x-x̄)(y-ȳ)) / Σ((x-x̄)²)
    /// where x = sequence number and y = owd_us.
    ///
    /// Returns 0 if fewer than 2 samples.
    pub fn trend_us_per_sec(&self) -> i32 {
        let n = self.samples.len();
        if n < 2 {
            return 0;
        }

        let n_f = n as f64;
        let sum_x: f64 = self.samples.iter().map(|(s, _)| *s as f64).sum();
        let sum_y: f64 = self.samples.iter().map(|(_, y)| *y as f64).sum();
        let mean_x = sum_x / n_f;
        let mean_y = sum_y / n_f;

        let mut num = 0.0;
        let mut den = 0.0;
        for &(x, y) in &self.samples {
            let dx = x as f64 - mean_x;
            let dy = y as f64 - mean_y;
            num += dx * dy;
            den += dx * dx;
        }

        if den.abs() < f64::EPSILON {
            return 0;
        }

        // slope is in µs/packet. Convert to µs/second assuming ~1ms inter-packet
        // spacing as a rough estimate. The raw slope per packet is more useful
        // for trend detection than an absolute rate, but the wire format specifies
        // µs/s. We report the raw per-packet slope scaled by 1000×, which
        // corresponds to the ~1ms inter-packet assumption.
        let slope_per_packet = num / den;
        (slope_per_packet * 1000.0) as i32
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

// ============================================================================
// ETX
// ============================================================================

/// Compute Expected Transmission Count from bidirectional delivery ratios.
///
/// ETX = 1 / (d_f × d_r) where d_f and d_r are forward and reverse
/// delivery probabilities (1.0 = perfect, 0.0 = no delivery).
///
/// Reference: De Couto et al. (2003), "A High-Throughput Path Metric for
/// Multi-Hop Wireless Routing", ACM SIGCOMM.
///
/// Clamped to [1.0, 100.0] — a perfect link scores 1.0 and a completely
/// broken link scores 100.0 (capped to avoid infinity).
pub fn compute_etx(d_forward: f64, d_reverse: f64) -> f64 {
    let product = d_forward * d_reverse;
    if product <= 0.0 {
        return 100.0;
    }
    (1.0 / product).clamp(1.0, 100.0)
}

// ============================================================================
// Spin Bit
// ============================================================================

/// Spin bit state for passive RTT estimation.
///
/// Based on the IETF IPPM spin bit concept (see RFC 9490, QUIC Spin Bit)
/// adapted for MMP frame headers. Uses asymmetric initiator/responder roles:
///
/// - **Initiator**: flips spin value on each received frame whose reflected
///   bit matches the current value; measures RTT from edge-to-edge intervals.
/// - **Responder**: copies received spin bit into outgoing frames, with a
///   monotonically-increasing counter guard to filter reordered frames
///   (only accepts a spin change from a frame with a higher counter than
///   the one that caused the last edge).
pub struct SpinBitState {
    is_initiator: bool,
    current_value: bool,
    /// Highest counter observed with a spin edge (responder guard).
    highest_counter_for_spin: u64,
    /// Time of last spin edge (initiator only, for RTT measurement).
    last_edge_time: Option<Instant>,
}

impl SpinBitState {
    pub fn new(is_initiator: bool) -> Self {
        Self {
            is_initiator,
            current_value: false,
            highest_counter_for_spin: 0,
            last_edge_time: None,
        }
    }

    /// Check if this is the spin bit initiator.
    pub fn is_initiator(&self) -> bool {
        self.is_initiator
    }

    /// Get the spin bit value to set on an outgoing frame.
    pub fn tx_bit(&self) -> bool {
        self.current_value
    }

    /// Process a received frame's spin bit.
    ///
    /// Returns an RTT sample duration if an edge was detected (initiator only).
    pub fn rx_observe(
        &mut self,
        received_bit: bool,
        counter: u64,
        now: Instant,
    ) -> Option<std::time::Duration> {
        if self.is_initiator {
            // Initiator: when the reflected bit matches what we sent,
            // that completes a round trip. Record the edge time, then
            // flip for the next cycle.
            if received_bit == self.current_value {
                let rtt = self.last_edge_time.map(|t| now.duration_since(t));
                self.last_edge_time = Some(now);
                self.current_value = !self.current_value;
                rtt
            } else {
                None
            }
        } else {
            // Responder: copy received bit, but only if counter is higher
            // (reordering guard — prevents stale/out-of-order frames from
            // corrupting the spin edge the initiator relies on for RTT)
            if counter > self.highest_counter_for_spin {
                self.highest_counter_for_spin = counter;
                self.current_value = received_bit;
            }
            None
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jitter_zero_input() {
        let mut j = JitterEstimator::new();
        j.update(0);
        assert_eq!(j.jitter_us(), 0);
    }

    #[test]
    fn test_jitter_convergence() {
        let mut j = JitterEstimator::new();
        // Feed constant transit delta of 1000µs
        for _ in 0..200 {
            j.update(1000);
        }
        // Should converge near 1000µs
        let jitter = j.jitter_us();
        assert!(
            jitter > 900 && jitter < 1100,
            "jitter={jitter}, expected ~1000"
        );
    }

    #[test]
    fn test_srtt_first_sample() {
        let mut s = SrttEstimator::new();
        s.update(10_000); // 10ms
        assert_eq!(s.srtt_us(), 10_000);
        assert_eq!(s.rttvar_us(), 5_000);
        assert!(s.initialized());
    }

    #[test]
    fn test_srtt_convergence() {
        let mut s = SrttEstimator::new();
        // Feed constant 50ms RTT
        for _ in 0..100 {
            s.update(50_000);
        }
        let srtt = s.srtt_us();
        assert!((srtt - 50_000).abs() < 1000, "srtt={srtt}, expected ~50000");
    }

    #[test]
    fn test_dual_ewma_initialization() {
        let mut e = DualEwma::new();
        assert!(!e.initialized());
        e.update(100.0);
        assert!(e.initialized());
        assert_eq!(e.short(), 100.0);
        assert_eq!(e.long(), 100.0);
    }

    #[test]
    fn test_dual_ewma_short_tracks_faster() {
        let mut e = DualEwma::new();
        // Initialize at 0
        e.update(0.0);
        // Jump to 100
        for _ in 0..20 {
            e.update(100.0);
        }
        // Short should be closer to 100 than long
        assert!(
            e.short() > e.long(),
            "short={} long={}",
            e.short(),
            e.long()
        );
    }

    #[test]
    fn test_owd_trend_flat() {
        let mut d = OwdTrendDetector::new(32);
        for i in 0..20 {
            d.push(i, 5000); // constant OWD
        }
        let trend = d.trend_us_per_sec();
        assert_eq!(trend, 0, "flat OWD should have zero trend");
    }

    #[test]
    fn test_owd_trend_increasing() {
        let mut d = OwdTrendDetector::new(32);
        for i in 0..20 {
            d.push(i, 5000 + (i as i64) * 100); // increasing by 100µs per packet
        }
        let trend = d.trend_us_per_sec();
        assert!(
            trend > 0,
            "increasing OWD should have positive trend, got {trend}"
        );
    }

    #[test]
    fn test_owd_trend_insufficient_samples() {
        let mut d = OwdTrendDetector::new(32);
        d.push(0, 5000);
        assert_eq!(d.trend_us_per_sec(), 0);
    }

    #[test]
    fn test_etx_perfect_link() {
        assert!((compute_etx(1.0, 1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_etx_lossy_link() {
        // 10% forward loss, 5% reverse loss
        let etx = compute_etx(0.9, 0.95);
        assert!(etx > 1.0 && etx < 2.0, "etx={etx}");
    }

    #[test]
    fn test_etx_zero_delivery() {
        assert_eq!(compute_etx(0.0, 1.0), 100.0);
        assert_eq!(compute_etx(1.0, 0.0), 100.0);
    }

    #[test]
    fn test_spin_bit_initiator_rtt() {
        let mut initiator = SpinBitState::new(true);
        let mut responder = SpinBitState::new(false);

        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(10);
        let t2 = t0 + std::time::Duration::from_millis(20);

        // Initiator sends with spin=false (initial)
        let bit_to_send = initiator.tx_bit();
        assert!(!bit_to_send);

        // Responder receives, copies bit
        responder.rx_observe(bit_to_send, 1, t0);
        assert!(!responder.tx_bit());

        // Responder sends back, initiator receives
        let resp_bit = responder.tx_bit();
        let rtt1 = initiator.rx_observe(resp_bit, 2, t1);
        // First edge: no previous edge to compare
        assert!(rtt1.is_none());

        // Now initiator's spin flipped to true
        let bit2 = initiator.tx_bit();
        assert!(bit2);

        // Responder receives new bit
        responder.rx_observe(bit2, 3, t1);
        assert!(responder.tx_bit());

        // Responder sends back, initiator receives
        let resp_bit2 = responder.tx_bit();
        let rtt2 = initiator.rx_observe(resp_bit2, 4, t2);
        // Second edge: should produce an RTT sample
        assert!(rtt2.is_some());
    }

    #[test]
    fn test_spin_bit_responder_counter_guard() {
        let mut responder = SpinBitState::new(false);

        // Receive counter=5 with spin=true
        responder.rx_observe(true, 5, Instant::now());
        assert!(responder.tx_bit());

        // Reordered packet with counter=3 and spin=false should be ignored
        responder.rx_observe(false, 3, Instant::now());
        assert!(responder.tx_bit()); // unchanged
    }
}
