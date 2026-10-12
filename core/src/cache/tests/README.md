# Coordinate cache tests

Unit tests for the coordinate cache one level up. Each file is a child
module of the file it tests, so it sees that file's private items and its
tests are named after it.

- `mod.rs`: tests for `mod.rs` (`CacheStats` and `CacheError`), declared
  there as `#[cfg(test)] mod tests;`; names `cache::tests::...`.
- `coord_cache.rs`: tests for `coord_cache.rs` (`CoordCache` insertion,
  lookup, expiry, eviction, invalidation, and the hint and verification
  rules), attached from there with
  `#[cfg(test)] #[path = "tests/coord_cache.rs"] mod tests;`; names
  `cache::coord_cache::tests::...`.
- `entry.rs`: tests for `entry.rs` (`CacheEntry` and its expiry),
  attached from there with `#[cfg(test)] #[path = "tests/entry.rs"] mod
  tests;`; names `cache::entry::tests::...`.
