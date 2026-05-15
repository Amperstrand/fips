// Ported from microfips: crates/microfips-protocol/src/peer_policy.rs
// Only frame rate limiting logic is included.

use std::time::Instant;

/// Sliding window duration for frame rate limiting (1 second).
pub const FRAME_RATE_WINDOW_MS: u64 = 1_000;

/// Maximum frames allowed within one window.
pub const FRAME_RATE_MAX: u16 = 100;

/// Simplified peer policy containing only frame rate limiting.
pub struct PeerPolicy {
    frame_count: u16,
    frame_window_start: Instant,
}

impl PeerPolicy {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            frame_count: 0,
            frame_window_start: now,
        }
    }

    /// Check if a new frame is allowed under the rate limit.
    ///
    /// Uses a sliding window of `FRAME_RATE_WINDOW_MS` milliseconds.
    /// At most `FRAME_RATE_MAX` frames are allowed per window.
    /// Returns `true` if the frame is allowed, `false` if rate-limited.
    // Ported from microfips
    pub fn check_frame_rate(&mut self) -> bool {
        let now = Instant::now();
        let elapsed_ms = now
            .duration_since(self.frame_window_start)
            .as_millis() as u64;

        if elapsed_ms >= FRAME_RATE_WINDOW_MS {
            self.frame_window_start = now;
            self.frame_count = 0;
        }

        if self.frame_count >= FRAME_RATE_MAX {
            self.frame_window_start = now;
            self.frame_count = 0;
            return false;
        }

        self.frame_count = self.frame_count.saturating_add(1);
        true
    }
}

impl Default for PeerPolicy {
    fn default() -> Self {
        Self::new()
    }
}
