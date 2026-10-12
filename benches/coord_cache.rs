//! Coordinate cache lookups, as the forwarding path makes them.
//!
//! `CoordCache::get` runs for each routed packet (`cached_coords` in the
//! routing view) and in session and probe handling. Hits and misses at a
//! small cache and at the default capacity, 50,000 entries.
//!
//! Only API that exists before and after the cache moved into `fips-core` is
//! used, so the same file runs against both trees for comparison.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use fips::{CoordCache, NodeAddr, TreeCoordinate};

fn addr(seed: &mut u64) -> NodeAddr {
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        chunk.copy_from_slice(&seed.to_le_bytes());
    }
    NodeAddr::from_bytes(bytes)
}

fn lookups(c: &mut Criterion) {
    let mut g = c.benchmark_group("coord_cache");
    for n in [1_000usize, 50_000] {
        let mut seed = 0x1234_5678_9abc_def1u64;
        let mut cache = CoordCache::new(n, u64::MAX / 2);
        let keys: Vec<NodeAddr> = (0..n).map(|_| addr(&mut seed)).collect();
        for key in &keys {
            let path: Vec<NodeAddr> = (0..5).map(|_| addr(&mut seed)).collect();
            let mut coords = vec![*key];
            coords.extend(path);
            let _ = cache.insert(*key, TreeCoordinate::from_addrs(coords).unwrap(), 1);
        }
        let hits: Vec<NodeAddr> = (0..4096).map(|i| keys[(i * 7919) % n]).collect();
        let misses: Vec<NodeAddr> = (0..4096).map(|_| addr(&mut seed)).collect();
        g.bench_with_input(BenchmarkId::new("get_hit", n), &n, |b, _| {
            let mut i = 0;
            b.iter(|| {
                i = (i + 1) % hits.len();
                black_box(cache.get(&hits[i], 2))
            })
        });
        g.bench_with_input(BenchmarkId::new("get_miss", n), &n, |b, _| {
            let mut i = 0;
            b.iter(|| {
                i = (i + 1) % misses.len();
                black_box(cache.get(&misses[i], 2))
            })
        });
    }
    g.finish();
}

criterion_group!(benches, lookups);
criterion_main!(benches);
