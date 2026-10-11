# core/src/noise

The fixed sizes of Noise handshake messages and their parts. The
link-layer wire codec in `proto/fmp/wire.rs` checks frame lengths
against them.

The handshake itself, the sessions, the replay window and the
encryption stay in the root crate's `noise` module for now, which
re-exports these constants, so `fips::noise::TAG_SIZE` and the rest are
unchanged. The root crate's XK message sizes are built from them there.

## Files

- `mod.rs`: six `usize` constants. `TAG_SIZE` (the AEAD tag),
  `PUBKEY_SIZE` (a compressed secp256k1 key) and `EPOCH_SIZE` (the
  startup epoch) are the parts; `EPOCH_ENCRYPTED_SIZE`,
  `HANDSHAKE_MSG1_SIZE` and `HANDSHAKE_MSG2_SIZE` are built from them.

## Invariants

- **The sizes are wire format.** Every node must agree on them, and they
  do not change on the deployed lines. A new handshake message adds a
  constant rather than changing one.
- **Only sizes are here.** No cryptography, keys or session state.
