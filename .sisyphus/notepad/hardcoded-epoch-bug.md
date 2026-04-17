# Hardcoded Epoch Bug Explainer

## What is it?

The **Epoch** is an 8-byte identifier in the FMP handshake protocol that distinguishes different connection attempts between the same pair of nodes.

## The Problem

### Microfips (ESP32/STM32)
```rust
// crates/microfips-protocol/src/node.rs:171
let epoch: [u8; noise::EPOCH_SIZE] = [0x01, 0, 0, 0, 0, 0, 0, 0];
```

The epoch is **hardcoded** to `[0x01, 0, 0, 0, 0, 0, 0, 0]` for **every** handshake attempt.

### FIPS (Linux/macOS)
```rust
// src/node/handshake/handshake.rs
let epoch = self.epoch_counter.fetch_add(1);
self.epoch_counter.store(epoch, Ordering::Relaxed);
```

FIPS **increments** the epoch counter for each outbound handshake, generating unique epochs like `[0x01, 0, 0, 0, 0, 0, 0, 1]`, `[0x01, 0, 0, 0, 0, 0, 0, 2]`, etc.

## Why This Causes Failures

### Normal Flow (First Attempt)
1. Microfips sends MSG1 with epoch `[0x01, 0, 0, 0, 0, 0, 0, 0]`
2. FIPS receives MSG1, caches response keyed by epoch
3. FIPS generates MSG2 with new ephemeral key, sends to microfips
4. Microfips decrypts with matching Noise state → **SUCCESS**

### Retry Flow (Subsequent Attempts)
1. Microfips retries (timeout or connection loss)
2. Microfips generates **new ephemeral key** (Noise state reset)
3. Microfips sends MSG1 with **same epoch** `[0x01, 0, 0, 0, 0, 0, 0, 0]`
4. FIPS sees duplicate epoch in cache
5. FIPS **re-sends cached MSG2** (optimization to avoid recomputation)
6. Microfips tries to decrypt with **new Noise state**
7. **DECRYPTION FAILS** - Noise state doesn't match the cached MSG2's ephemeral key
8. Connection fails
9. Repeat steps 1-8 for ~30 seconds until FIPS link-dead timeout clears cache

### Why 30-Second Delay?
FIPS has a 30-second link-dead timeout. When a peer appears unreachable, FIPS keeps the cached handshake responses for that long. Only after the timeout expires does FIPS clear the cache and accept a "fresh" handshake.

## The Fix

### Microfips Changes Required
```rust
// crates/microfips-protocol/src/node.rs

pub struct Node {
    // ... existing fields ...
    epoch_counter: u64,  // ADD THIS
}

impl Node {
    pub fn session(&mut self, ...) -> Result<()> {
        // Increment epoch for each attempt
        self.epoch_counter = self.epoch_counter.wrapping_add(1);
        let epoch_bytes = self.epoch_counter.to_le_bytes();
        let epoch: [u8; noise::EPOCH_SIZE] = [
            epoch_bytes[0], epoch_bytes[1], epoch_bytes[2], epoch_bytes[3],
            epoch_bytes[4], epoch_bytes[5], epoch_bytes[6], epoch_bytes[7],
        ];
        
        // ... rest of handshake ...
    }
}
```

### FIPS Changes Required
**None** - FIPS already generates unique epochs correctly.

## Does This Require Changes to Both?

| Project | Changes Required | Why |
|----------|------------------|-----|
| **microfips** | **YES** | Must increment epoch counter |
| **FIPS** | **NO** | Already generates unique epochs |

## Testing Strategy

1. **Unit test**: Verify microfips increments epoch between attempts
2. **Integration test**: 
   - Start FIPS node
   - Connect microfips device
   - Force retry (kill connection after MSG1)
   - Verify retry succeeds within 1-2 seconds (not 30 seconds)
3. **Compatibility test**: 
   - Updated microfips → old FIPS (should work)
   - Old microfips → updated FIPS (should work)
   - Updated microfips → updated FIPS (should work)

## Impact on Current Work

This bug primarily affects **ESP32 WiFi connections** where handshakes may timeout and retry frequently. The STM32 USB CDC is less affected because the serial proxy delays packets enough that FIPS doesn't cache as aggressively.

For **BLE connections** (like our macOS ↔ Linux test), this is less of an issue because:
- BLE connections are more stable once established
- Fewer retries during normal operation
- The 30-second delay is only hit on initial connection failures
