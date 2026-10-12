use super::*;
use alloc::string::ToString;

#[test]
fn test_cache_stats_fill_ratio() {
    let stats = CacheStats {
        entries: 50,
        max_entries: 100,
        expired: 0,
        avg_age_ms: 0,
    };
    assert!((stats.fill_ratio() - 0.5).abs() < f64::EPSILON);
}

#[test]
fn test_cache_stats_fill_ratio_zero_max() {
    let stats = CacheStats {
        entries: 0,
        max_entries: 0,
        expired: 0,
        avg_age_ms: 0,
    };
    assert!((stats.fill_ratio() - 0.0).abs() < f64::EPSILON);
}

#[test]
fn test_cache_stats_fill_ratio_full() {
    let stats = CacheStats {
        entries: 100,
        max_entries: 100,
        expired: 10,
        avg_age_ms: 500,
    };
    assert!((stats.fill_ratio() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn test_cache_error_display() {
    let full = CacheError::CacheFull { max: 1000 };
    assert_eq!(full.to_string(), "cache full: max 1000 entries");

    let not_found = CacheError::NotFound;
    assert_eq!(not_found.to_string(), "entry not found");

    let expired = CacheError::Expired;
    assert_eq!(expired.to_string(), "entry expired");
}
