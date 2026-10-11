# core/src/transport

The transport value types: the identifiers, address and statistics that
a transport and the protocol cores both name. Nothing here sends or
receives; the transports themselves (UDP, TCP, Ethernet, BLE, Tor and
the rest) own sockets and devices and stay in the root crate.

The root crate re-exports these types under `fips::transport`, next to
the transports and the `Transport` trait, so callers there name them as
before. The protocol cores in `proto/` import them from here.

## Files

- `mod.rs`: declares `types` and re-exports its five public types.
- `types.rs`: `TransportId` (one transport instance), `LinkId` (one
  link), `LinkDirection` (who opened it), `TransportAddr` (an opaque
  address in the transport's own form) and `LinkStats` (a link's
  counters). `TransportAddr::from_socket_addr` is here too.
- `tests/`: the unit tests for `types.rs`; its README describes it.

## Invariants

- **Value types only.** No sockets, no clocks and no I/O. A helper that
  needs the operating system, such as resolving a host name or reading a
  socket's local address, stays in the root crate's `transport` module.
- **`from_socket_addr` does not allocate for formatting.** It writes the
  socket address's `Display` form into a 64-byte stack buffer through
  `core::fmt::Write`, then copies those bytes into the address. The
  longest form, a scoped IPv6 address with a ten-digit scope and the
  largest port, is 58 bytes.
- **`TransportAddr` compares by value.** Equality and hashing use the
  bytes, not the shared allocation, so two addresses built separately
  from the same bytes are equal. Clones share one immutable allocation.
