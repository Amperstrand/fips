use super::*;

#[test]
fn test_allocator_basic() {
    let mut alloc = IndexAllocator::new();
    assert!(alloc.is_empty());
    assert_eq!(alloc.count(), 0);

    let idx1 = alloc.allocate().unwrap();
    assert!(!alloc.is_empty());
    assert_eq!(alloc.count(), 1);
    assert!(alloc.is_allocated(idx1));

    let idx2 = alloc.allocate().unwrap();
    assert_eq!(alloc.count(), 2);
    assert!(alloc.is_allocated(idx2));
    assert_ne!(idx1, idx2);

    alloc.free(idx1).unwrap();
    assert_eq!(alloc.count(), 1);
    assert!(!alloc.is_allocated(idx1));
    assert!(alloc.is_allocated(idx2));
}

#[test]
fn test_allocator_free_not_found() {
    let mut alloc = IndexAllocator::new();
    let result = alloc.free(SessionIndex::new(12345));
    assert!(matches!(result, Err(IndexError::NotFound(12345))));
}

#[test]
fn test_allocator_reserve() {
    let mut alloc = IndexAllocator::new();

    let idx = SessionIndex::new(0xdeadbeef);
    alloc.reserve(idx).unwrap();
    assert!(alloc.is_allocated(idx));

    // Double reserve fails
    let result = alloc.reserve(idx);
    assert!(matches!(result, Err(IndexError::AlreadyInUse(0xdeadbeef))));
}

#[test]
fn test_allocator_uniqueness() {
    let mut alloc = IndexAllocator::new();
    let mut indices = Vec::new();

    // Allocate 1000 indices and verify all unique
    for _ in 0..1000 {
        let idx = alloc.allocate().unwrap();
        assert!(!indices.contains(&idx));
        indices.push(idx);
    }

    assert_eq!(alloc.count(), 1000);
}

#[test]
fn test_allocator_clear() {
    let mut alloc = IndexAllocator::new();

    for _ in 0..10 {
        alloc.allocate().unwrap();
    }
    assert_eq!(alloc.count(), 10);

    alloc.clear();
    assert!(alloc.is_empty());
    assert_eq!(alloc.count(), 0);
}

#[test]
fn test_allocator_reuse_after_free() {
    let mut alloc = IndexAllocator::new();

    let idx = alloc.allocate().unwrap();
    alloc.free(idx).unwrap();

    // The specific index might not be reused immediately (random),
    // but the count should allow allocation
    let idx2 = alloc.allocate().unwrap();
    assert_eq!(alloc.count(), 1);

    // Can now reserve the original index if it wasn't randomly reused
    if idx != idx2 {
        alloc.reserve(idx).unwrap();
    }
}
