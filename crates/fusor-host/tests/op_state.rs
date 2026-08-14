//! The centralized op-state registry: one owner-task singleton table
//! keyed by `TypeId`, replacing the scattered per-module thread_locals.

use fusor_host::ops::OpStateRegistry;

#[derive(Debug, Default)]
struct CounterState {
    bumps: u32,
}

#[derive(Debug, Default)]
struct ListState {
    entries: Vec<String>,
}

#[test]
fn a_state_installs_and_borrows_by_type() {
    assert!(!OpStateRegistry::has::<CounterState>());
    OpStateRegistry::install(CounterState { bumps: 3 }).expect("installed");
    assert!(OpStateRegistry::has::<CounterState>());
    assert_eq!(
        OpStateRegistry::with::<CounterState, _>(|state| state.bumps).expect("with"),
        3
    );
    OpStateRegistry::with_mut::<CounterState, _>(|state| state.bumps += 1).expect("with_mut");
    assert_eq!(
        OpStateRegistry::with::<CounterState, _>(|state| state.bumps).expect("with"),
        4
    );
    // A different type has its own slot.
    assert!(!OpStateRegistry::has::<ListState>());
}

#[test]
fn double_installation_returns_the_state_unchanged() {
    OpStateRegistry::install(CounterState::default()).expect("installed");
    let second = CounterState { bumps: 9 };
    let returned = OpStateRegistry::install(second).expect_err("slot taken");
    assert_eq!(returned.bumps, 9, "the rejected state comes back unchanged");
    assert_eq!(
        OpStateRegistry::with::<CounterState, _>(|state| state.bumps).expect("with"),
        0,
        "the original occupant is untouched"
    );
}

#[test]
fn a_missing_state_fails_closed_with_its_type_name() {
    assert!(!OpStateRegistry::has::<ListState>());
    let error = OpStateRegistry::with::<ListState, _>(|_| ()).expect_err("missing");
    assert!(
        error.to_string().contains("ListState"),
        "the diagnostic names the type: {error}"
    );
}

#[test]
fn take_removes_the_slot_and_returns_the_state() {
    OpStateRegistry::install(ListState {
        entries: vec!["a".to_owned()],
    })
    .expect("installed");
    let taken = OpStateRegistry::take::<ListState>().expect("taken");
    assert_eq!(taken.entries, vec!["a"]);
    assert!(!OpStateRegistry::has::<ListState>());
    assert!(OpStateRegistry::take::<ListState>().is_none());
}

#[test]
fn installs_are_per_thread() {
    // The registry is thread-local by design (single-owner op state):
    // this thread's install must not be visible to another thread.
    OpStateRegistry::install(CounterState { bumps: 7 }).expect("installed");
    let observed = std::thread::spawn(|| OpStateRegistry::has::<CounterState>())
        .join()
        .expect("joined");
    assert!(!observed, "other threads see their own (empty) registry");
}
