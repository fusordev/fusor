use std::sync::{Arc, Barrier};
use std::time::Duration;
use std::{thread, time::Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use sdd::{Guard, Owned, PrivateCollector, Shared};

fn guard_accelerate(c: &mut Criterion) {
    let _guard = Guard::new();
    c.bench_function("EBR: accelerate", |b| {
        b.iter(|| {
            let guard = Guard::new();
            guard.accelerate();
        })
    });
}

fn guard_single(c: &mut Criterion) {
    c.bench_function("EBR: guard", |b| {
        b.iter(|| {
            let _guard = Guard::new();
        })
    });
}

fn guard_superposed(c: &mut Criterion) {
    let _guard = Guard::new();
    c.bench_function("EBR: superposed guard", |b| {
        b.iter(|| {
            let _guard = Guard::new();
        })
    });
}

fn guard_traversal_8(c: &mut Criterion) {
    c.bench_function("EBR: traverse 16 collectors", |b| {
        b.iter_custom(|n| test_traverse(16, n))
    });
}

fn guard_traversal_64(c: &mut Criterion) {
    c.bench_function("EBR: traverse 256 collectors", |b| {
        b.iter_custom(|n| test_traverse(256, n))
    });
}

fn owned_allocate(c: &mut Criterion) {
    c.bench_function("Owned: allocate", |b| {
        b.iter(|| {
            let owned = Owned::new([0u64; 8]);
            drop(owned);
        })
    });

    for _ in 0..4 {
        Guard::new().accelerate();
    }
}

fn shared_allocate(c: &mut Criterion) {
    c.bench_function("Shared: allocate", |b| {
        b.iter(|| {
            let shared = Shared::new([0u64; 8]);
            drop(shared);
        })
    });

    for _ in 0..4 {
        Guard::new().accelerate();
    }
}

fn private_collector_push(c: &mut Criterion) {
    let private_collector = PrivateCollector::new();
    unsafe {
        private_collector.collect_owned(Owned::new(()), &Guard::new());
    }
    c.bench_function("PrivateCollector: push", |b| {
        b.iter_custom(|n| {
            let mut owned_vec = Vec::with_capacity(n as usize);
            for _ in 0..n {
                owned_vec.push(Owned::new(()));
            }
            let guard = Guard::new();
            let start = Instant::now();
            while let Some(owned) = owned_vec.pop() {
                unsafe {
                    private_collector.collect_owned(owned, &guard);
                }
            }
            let elapsed = start.elapsed();
            drop(owned_vec);
            guard.accelerate();
            elapsed
        })
    });
}

fn test_traverse(n: usize, iter: u64) -> Duration {
    let barrier = Arc::new(Barrier::new(n + 1));
    let mut threads = Vec::with_capacity(n);
    for _ in 0..n {
        let barrier = barrier.clone();
        threads.push(thread::spawn(move || {
            let guard = Guard::new();
            drop(guard);
            barrier.wait();
            barrier.wait();
        }));
    }
    let guard = Guard::new();
    drop(guard);
    barrier.wait();

    let start = Instant::now();
    for _ in 0..iter {
        let guard = Guard::new();
        guard.accelerate();
        drop(guard);
    }
    let elapsed = start.elapsed();

    barrier.wait();
    for t in threads {
        assert!(t.join().is_ok());
    }
    for _ in 0..4 {
        let guard = Guard::new();
        guard.accelerate();
    }
    elapsed
}

criterion_group!(
    ebr,
    guard_accelerate,
    guard_single,
    guard_superposed,
    guard_traversal_8,
    guard_traversal_64,
    owned_allocate,
    private_collector_push,
    shared_allocate,
);
criterion_main!(ebr);
