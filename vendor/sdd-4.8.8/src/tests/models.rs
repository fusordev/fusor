use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};

use loom::sync::atomic::{AtomicBool, AtomicUsize};
use loom::thread::{spawn, yield_now};

use crate::{AtomicOwned, AtomicShared, Guard, Owned, PrivateCollector};

struct A(AtomicBool, Arc<AtomicUsize>);
impl Drop for A {
    fn drop(&mut self) {
        self.0.store(true, Relaxed);
        self.1.fetch_add(1, Relaxed);
    }
}

static SERIALIZER: Mutex<()> = Mutex::new(());

#[test]
fn ebr_owned() {
    let _guard = SERIALIZER.lock().unwrap();
    loom::model(|| {
        let drop_count = Arc::new(AtomicUsize::new(0));
        let data_owned = AtomicOwned::new(A(AtomicBool::new(false), drop_count.clone()));

        let guard = Guard::new();
        let ptr = data_owned.load(Relaxed, &guard);

        let thread = spawn(move || {
            let guard = Guard::new();
            guard.accelerate();

            let ptr = data_owned.load(Relaxed, &guard);
            drop(data_owned);

            assert!(!ptr.as_ref().unwrap().0.load(Relaxed));
            drop(guard);

            while drop_count.load(Relaxed) != 1 {
                Guard::new().accelerate();
                yield_now();
            }
        });

        assert!(!ptr.as_ref().unwrap().0.load(Relaxed));
        drop(guard);

        assert!(thread.join().is_ok());
    });
}

#[test]
fn ebr_shared() {
    let _guard = SERIALIZER.lock().unwrap();
    loom::model(|| {
        let drop_count = Arc::new(AtomicUsize::new(0));
        let data_shared = AtomicShared::new(A(AtomicBool::new(false), drop_count.clone()));

        let guard = Guard::new();
        let ptr = data_shared.load(Relaxed, &guard);

        let thread = spawn(move || {
            let data_shared_clone = data_shared.get_shared(Relaxed, &Guard::new()).unwrap();
            drop(data_shared);

            let guard = Guard::new();
            guard.accelerate();

            let ptr = data_shared_clone.get_guarded_ptr(&guard);
            drop(data_shared_clone);

            assert!(!ptr.as_ref().unwrap().0.load(Relaxed));
            drop(guard);

            while drop_count.load(Relaxed) != 1 {
                Guard::new().accelerate();
                yield_now();
            }
        });

        assert!(!ptr.as_ref().unwrap().0.load(Relaxed));
        drop(guard);

        assert!(thread.join().is_ok());
    });
}

#[test]
fn private_collector() {
    let _guard = SERIALIZER.lock().unwrap();
    loom::model(|| {
        let drop_count = Arc::new(AtomicUsize::new(0));
        let data_owned = Owned::new(A(AtomicBool::new(false), drop_count.clone()));

        let thread = spawn(move || {
            let private_collector = PrivateCollector::new();
            unsafe {
                private_collector.collect_owned(data_owned, &Guard::new());
            }
            private_collector
        });

        assert_eq!(drop_count.load(Relaxed), 0);
        let private_collector = thread.join().unwrap();
        drop(private_collector);
        assert_eq!(drop_count.load(Relaxed), 1);
    });
}
