# core/src/cache

The coordinate cache: a map from node address to tree coordinates, so a
data packet can be routed without carrying its destination's
coordinates. Session setup and discovery fill it; the forwarding path
reads it for each routed packet, and session and probe handling read it
too.

The root crate re-exports it under `fips::cache` from `src/cache.rs`,
and names the cache through a type alias,
`fips::cache::CoordCache = fips_core::cache::CoordCache<OsEntropy>`, so
its callers name it as before.

## Files

- `mod.rs`: declares the submodules and re-exports their public items;
  `CacheError` and `CacheStats`.
- `coord_cache.rs`: `CoordCache`: insertion from hints and from verified
  lookups, lookup with and without refreshing an entry, invalidation,
  expiry and eviction; the default size (50,000 entries) and TTL;
  `HintOutcome`, what a hint write did.
- `entry.rs`: `CacheEntry`, one cached coordinate with its times, path
  MTU and source; `CoordSource` (`Verified` or `Hint`);
  `VERIFIED_TTL_MS`.
- `tests/`: the unit tests for the files here; its README describes it.

## Invariants

- **The map is keyed from the host's entropy source.** `CoordCache<E>`
  builds its map, a `utils::hash::HashMap`, with a hasher keyed from `E`
  at construction. A peer chooses the node addresses it sends, so a key
  it could predict would let it steer where entries land. Iteration
  order is random per process as a result, and two things follow it:
  which of several equally eligible entries eviction picks, and the row
  order of the cache snapshot the root crate reports. Neither may come
  to depend on address order.
- **Time is an argument.** Every method that needs the time takes
  `current_time_ms: u64`, in Unix milliseconds; nothing here reads a
  clock.
- **A hint never displaces a live verified entry.** A coordinate copied
  off a passing packet is a hint and is written with `insert`; one from
  a lookup whose proof was checked is written with `insert_verified`.
  A verified entry outranks hints for `VERIFIED_TTL_MS` from when it was
  verified, on a clock that use never refreshes. The entry's own TTL is
  extended by traffic, so if verification shared it a once-verified
  entry would refuse every hint forever, including the ones that carry a
  destination's real move. After the window the entry stays usable but
  a hint may replace it.
- **Eviction never makes room for a hint at a verified entry's
  expense.** When the cache is full, eviction removes an expired entry
  if there is one, otherwise the least recently used entry that is not
  live-verified. If every entry is live-verified it removes nothing, and
  the hint is refused rather than the cache growing past its cap.
  Otherwise a flood of hints could age a verified entry into the
  eviction slot and then plant a value in the space it left.
