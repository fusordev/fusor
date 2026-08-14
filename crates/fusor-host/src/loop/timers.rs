//! Timer and `setImmediate` state (§6.4): a deadline-ordered heap plus the
//! per-turn immediate queue, hosted by [`super::HostLoop`].

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use fusor_runtime::JsValue;

/// A monotonically increasing, never-reused timer identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TimerId(u32);

impl TimerId {
    /// Reconstructs an id from the numeric value JavaScript supplies.
    #[must_use]
    pub const fn from_u32(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric id JavaScript observes and supplies to the
    /// `clearTimeout`/`clearInterval`/`clearImmediate` ops.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// One registered timer callback.
#[derive(Debug)]
pub(crate) struct TimerCallback {
    /// The JavaScript callback invoked with the job-callback semantics.
    pub callback: JsValue,
    /// Whether the timer re-arms after firing.
    pub repeating: bool,
    /// The delay used for re-arming, in milliseconds (already normalized:
    /// truncated toward zero and negative values clamped to 0, §6.4).
    pub delay: Duration,
}

/// One deadline entry in the ordering heap.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TimerEntry {
    pub deadline: Instant,
    pub sequence: u64,
    pub id: TimerId,
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering: earlier deadlines and smaller sequences are
        // "greater" so the BinaryHeap pops them first.
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| other.sequence.cmp(&self.sequence))
            .then_with(|| other.id.0.cmp(&self.id.0))
    }
}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The loop-owned timer and immediate state, installed on the owner task.
#[derive(Debug)]
pub(crate) struct TimerState {
    pub heap: BinaryHeap<TimerEntry>,
    pub callbacks: HashMap<TimerId, TimerCallback>,
    /// `setImmediate` queue: run after the current turn's events, before the
    /// host-job drain (§6.4).
    pub immediates: VecDeque<TimerId>,
    pub next_id: u32,
    pub sequence: u64,
    /// The virtual clock (§6.4, §12.2); the loop advances it deterministically.
    pub now: Instant,
}

impl Default for TimerState {
    fn default() -> Self {
        Self {
            heap: BinaryHeap::new(),
            callbacks: HashMap::new(),
            immediates: VecDeque::new(),
            next_id: 0,
            sequence: 0,
            now: Instant::now(),
        }
    }
}

impl TimerState {
    /// Registers one timer and returns its identity.
    pub fn push(&mut self, callback: TimerCallback, deadline: Instant) -> TimerId {
        let id = TimerId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let entry = TimerEntry {
            deadline,
            sequence: self.sequence,
            id,
        };
        self.sequence = self.sequence.saturating_add(1);
        self.heap.push(entry);
        self.callbacks.insert(id, callback);
        id
    }

    /// Registers one `setImmediate` callback.
    pub fn push_immediate(&mut self, callback: TimerCallback) -> TimerId {
        let id = TimerId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.callbacks.insert(id, callback);
        self.immediates.push_back(id);
        id
    }

    /// Removes a timer without firing it; returns whether it existed.
    pub fn cancel(&mut self, id: TimerId) -> bool {
        self.callbacks.remove(&id).is_some()
    }

    /// Returns the next deadline, if any timers remain.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.heap.peek().map(|entry| entry.deadline)
    }

    /// Returns whether any timer or immediate remains.
    pub fn has_pending(&self) -> bool {
        !self.callbacks.is_empty()
    }
}

/// Installs a fresh timer state for one [`super::HostLoop`] into the
/// op-state registry (owner-task bootstrap).
///
/// # Errors
///
/// Returns the state unchanged when one is already installed.
pub(crate) fn install_timer_state(state: TimerState) -> Result<(), TimerState> {
    crate::ops::OpStateRegistry::install(state)
}

/// Removes the installed timer state (shutdown teardown, §7.4), dropping
/// the pending callbacks with it so a fresh loop can install on the same
/// thread.
#[must_use]
pub(crate) fn take_timer_state() -> Option<TimerState> {
    crate::ops::OpStateRegistry::take::<TimerState>()
}

/// Borrows the installed timer state mutably (the op entry points).
pub(crate) fn with_timer_state<R>(
    operation: impl FnOnce(&mut TimerState) -> R,
) -> Result<R, crate::ops::OpStateError> {
    crate::ops::OpStateRegistry::with_mut::<TimerState, R>(operation)
}
