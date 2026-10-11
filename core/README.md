# fips-core

The protocol core of FIPS: the code that decides what a node does, kept
free of everything that ties it to an operating system or a runtime.
The goal is one implementation of the protocol that runs unchanged on a
Linux daemon, an Android app and a microcontroller, with each host
supplying its own I/O around it.

The crate is a workspace member next to the root `fips` package. Code
moves in from the root crate one module at a time. So far it holds:

- the identity types: `NodeAddr`, `Ipv6Address`, `PeerIdentity`, the
  npub and nsec encodings and `IdentityError`, re-exported by the root
  crate under `fips::identity`. `Identity`, which signs, stays in the
  root crate until randomness can be passed in;
- the transport value types: `TransportId`, `LinkId`, `LinkDirection`,
  `TransportAddr` and `LinkStats`, re-exported under `fips::transport`.
  The transports themselves stay in the root crate;
- `SessionIndex`, re-exported under `fips::utils::index`. Its random
  allocator stays in the root crate;
- the Noise handshake message sizes, re-exported under `fips::noise`.
  The handshake itself stays in the root crate;
- the protocol itself, `proto`: the wire codecs and sans-IO state
  machines of every layer (link, session, spanning tree, bloom filters,
  routing, lookups, metrics, probes). The root crate reaches it as
  `crate::proto` and re-exports its public types at its crate root.

## What belongs here

Code whose behaviour is the same on every target:

- identity types and their encodings (node addresses, npub and nsec);
- wire formats: parsing and serializing frames and messages;
- protocol state machines: handshakes, sessions, the spanning tree,
  bloom filters, routing decisions, lookups;
- policy and bookkeeping that is pure computation: rate limits, caches,
  statistics;
- the traits that describe what the core needs from its host, such as a
  transport to send on.

What does not belong here: sockets, TUN devices, files, configuration
parsing, logging, signal handling, threads, timers and anything that
waits. Those stay in the root `fips` crate, which drives the core.

## The rules

These are enforced by the compiler where it can, and by review where it
cannot.

**`no_std` with `alloc`.** The crate is `#![no_std]` and uses
`extern crate alloc`. Use `core::` and `alloc::` paths (`core::fmt`,
`core::net::Ipv6Addr`, `alloc::vec::Vec`, `alloc::string::String`).
`std::collections::HashMap` and `HashSet` are not available; use
`alloc::collections::BTreeMap` and `BTreeSet`, or propose a `no_std`
hash map dependency in review.

**Sans-IO.** The core never performs I/O. It takes inputs (bytes that
arrived, the current time, an event) and returns values that describe
what the caller must do: bytes to send, a timer to arm, a link to close,
an event to report. The caller owns the socket, the clock and the
scheduler, and carries out what the core returned. A typical shape is a
method that takes `&mut self` plus the input and returns an action enum
or a small list of actions.

**No async.** No `async fn`, no futures, no `.await`, and no dependency
on tokio or any other runtime. The core is synchronous, and every call
returns promptly. Waiting is expressed as an action ("call me back at
time T"), not as a suspended task.

**Time is an argument.** Never read a clock. Functions that need the
time take it as a parameter, and timeouts are deadlines the caller
schedules. This is what makes the state machines testable with
synthetic timestamps.

**Randomness is an argument.** Never reach for a thread-local or OS
random source. Code that needs random bytes or a nonce takes an RNG or
the bytes from its caller. The shared secp256k1 context in `identity`
follows this too: the host blinds it by calling
`identity::init_secp(seed)` at start-up with 32 bytes from its own
random source, as the daemon does.

**No OS calls and no target `cfg`.** No `cfg(target_os = ...)`,
`cfg(unix)`, `cfg(windows)` or similar. If behaviour differs by
platform, the difference lives in the host crate and reaches the core
through a parameter or a trait.

**No logging.** No `tracing`, `log` or `println!`. Report what happened
in the return value (an event, an error, a counter in a stats struct)
and let the caller decide what to log. Decisions belong to the core;
observability and I/O belong to the host.

**Avoid global state.** `std::sync::LazyLock`, `Mutex` and
`thread_local!` do not exist here. Prefer passing state in. Where a
shared, lazily built value is unavoidable, `once_cell::race::OnceBox`
works without `std`.

**No 64-bit std atomics.** `AtomicU64` and `AtomicI64` from
`core::sync::atomic` do not exist on the 32-bit MIPS targets the
OpenWrt packages cover. Use `portable_atomic::AtomicU64`.
`testing/check-portable-atomics.py` checks this crate as well as the
root.

**Peer input is untrusted.** Bytes from the network never cause a
panic: no `unwrap`, indexing or arithmetic that a malformed or hostile
frame can make fail. Return an error. Bound every allocation by a limit
the core controls, not by a length field the peer chose.

## Dependencies

Add a dependency with `default-features = false`, then enable `alloc`
(or the crate's equivalent) and nothing else it does not need. A new
dependency must build without `std`.

`libm` is the one dependency left on its default features, and it is
not a precedent: its only default is `arch`, kept so the bloom
false-positive estimate uses the implementation the root crate used
before the move.

The workspace build unifies features across members, so a feature the
root crate turns on for its own use also applies to the core in a
normal `cargo build`. That can hide a feature the core needs but never
asked for. Check what the core gets on its own:

```bash
cargo tree -p fips-core -e features
```

If a dependency's `std` feature is unavoidable for now (for example, to
get `std::error::Error` impls on its error types), enable it in
`core/Cargo.toml` with a comment saying why. The core still cannot name
`std` itself, but that dependency will link it, and the comment marks
what has to change before the crate can build for a target without
`std`.

## Tests

Tests run on the host and may use `std`: `lib.rs` declares
`#[cfg(test)] extern crate std;`. That does not bring `std`'s prelude
macros into scope, so a test module that uses `format!` or `vec!` needs
`use alloc::format;` or `use alloc::vec;`.

Unit tests go in their own file, never in an inline `mod tests { ... }`
block, and they are kept per directory: a directory's tests are in its
`tests.rs` or its `tests/` directory, never both, declared from
`mod.rs` with `#[cfg(test)] mod tests;`. A file module's tests are
`tests/<file>.rs` in the same directory, attached from the source file
with `#[cfg(test)] #[path = "tests/<file>.rs"] mod tests;`, so they stay
a child module of that file, see its private items and keep their test
names. A single file's tests never get a directory of their own. If
`tests/<file>.rs` is already taken by the directory's own test module,
use another name, such as `tests/<file>_private.rs`.

`testutil.rs` holds fixtures several test modules share, and is compiled
for tests only. Dev-dependencies in `core/Cargo.toml` are built only for
the tests, which run on the host, so they may use `std`.

Test the state machines the way the host drives them: build the state,
feed it inputs with synthetic timestamps and fixed random bytes, and
assert on the actions it returns. No sockets, no sleeps and no real
clock.

## Moving code in from the root crate

- Move whole files with `git mv` so history follows them. When only
  part of a file belongs here, cut it in a commit of its own, as
  `SessionIndex` and the Noise sizes were, and keep the file moves pure
  renames.
- Leave a re-export at the old path in the root crate
  (`pub use fips_core::...;`), so existing `crate::...` imports keep
  resolving and the move changes no caller.
- Items the root crate still reaches become `pub`. A module whose whole
  surface the root drives, such as `proto`, stays documented, and
  trimming that surface is later work: hiding a module the root drives
  end to end would hide most of the crate from its own docs. Mark
  individual internals that are public only for the root
  `#[doc(hidden)]`, as `identity::secp` and `identity::sha256` are.
- An item only the root crate's tests reach is gated
  `#[cfg(any(test, feature = "test-hooks"))]`, because `cfg(test)` does
  not cross crates. The root enables `test-hooks` from its
  `[dev-dependencies]` only, so hooks reach only a build that compiles
  tests, examples or benches; a plain `cargo build`, release or not,
  leaves them out. Make the `cfg` change while the item is still in the
  root, so the move stays a pure rename.
- An inherent `impl` must live in the type's crate. Methods the root
  crate adds to a moved type move with it, or become a free function or
  an extension trait.
- Check `super::` imports as well as `crate::` ones when working out
  what a module depends on; both have to be satisfied inside the core.
- An intra-doc link to an item that stays in the root crate will not
  resolve from here. Write the name as plain text.
- Split the tests the same way: tests that use only moved items move
  with them, and the rest stay. Compare the test names before and after
  so none is lost.
- A module whose tests are inline has them extracted to their own file,
  as described under Tests, in a commit before the move.
- Every directory in this crate has a `README.md`: what the directory is
  for, what each file or subdirectory holds, how it relates to the
  levels above and below, and the rules or invariants someone changing
  it must know. Write it for a human reader and a coding agent alike.
  When code moves into a new directory, or a directory's contents
  change, add or update its README in the same change.
- A move changes no behaviour. Code that needs changing to meet the
  rules above (a clock read, a log line, a target `cfg`) is changed in a
  separate commit first, while it is still in the root crate.

## Checks

Both local CI (`testing/ci-local.sh`) and GitHub CI build, lint and
test this crate along with the root, because the workspace lists it in
`default-members`. They also build it on its own, under its own feature
resolution:

```bash
cargo clippy -p fips-core --all-targets -- -D warnings
cargo test -p fips-core
```

Run those two before sending a change here. The repository guards in
`testing/` (atomics, comment references, log strings) scan `core/src/`
as well as `src/`.

These checks run on the host. No CI job yet builds the crate for a
target without `std`, so a dependency that links `std` is not caught
automatically; the dependency comments described above are what track
it until that build exists.

## Versioning

The crate's version is fixed at `0.0.0` and is never bumped, and
`publish = false`. It is versioned with the root `fips` package, whose
`Cargo.toml` is the only place a release changes the version.
