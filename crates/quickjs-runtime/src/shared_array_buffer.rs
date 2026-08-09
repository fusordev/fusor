//! Thread-safe shared data blocks and specification-ordered Atomics waiters.

use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::mpsc::UnboundedSender;

use crate::{ExecutionError, RuntimeResource};

const WAITER_PENDING: u8 = 0;
const WAITER_NOTIFIED: u8 = 1;
const WAITER_TIMED_OUT: u8 = 2;
const WAITER_CANCELLED: u8 = 3;
static NEXT_WAITER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_WAKE_TOKEN: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_atomics_waiter_id() -> u64 {
    loop {
        let id = NEXT_WAITER_ID.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

pub(crate) fn next_atomics_wake_token() -> u64 {
    loop {
        let token = NEXT_WAKE_TOKEN.fetch_add(1, Ordering::Relaxed);
        if token != 0 {
            return token;
        }
    }
}

/// A cloneable, thread-safe host capability for one ECMAScript Shared Data
/// Block. Importing the handle into another runtime creates a new
/// `SharedArrayBuffer` object that aliases the same bytes and waiter lists.
#[derive(Clone)]
pub struct SharedArrayBufferHandle {
    pub(crate) block: Arc<SharedDataBlock>,
}

impl SharedArrayBufferHandle {
    /// Returns the Shared Data Block's current byte length.
    #[must_use]
    pub fn byte_length(&self) -> usize {
        self.block.byte_length()
    }

    /// Returns the maximum byte length of a growable Shared Data Block, or the
    /// current byte length for a fixed block.
    #[must_use]
    pub fn max_byte_length(&self) -> usize {
        self.block.max_byte_length()
    }

    /// Returns whether this Shared Data Block can grow.
    #[must_use]
    pub fn is_growable(&self) -> bool {
        self.block.is_growable()
    }
}

impl fmt::Debug for SharedArrayBufferHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedArrayBufferHandle")
            .field("byte_length", &self.byte_length())
            .field("max_byte_length", &self.max_byte_length())
            .field("growable", &self.is_growable())
            .finish_non_exhaustive()
    }
}

pub(crate) struct SharedDataBlock {
    state: Mutex<SharedDataBlockState>,
    max_byte_length: Option<usize>,
}

struct SharedDataBlockState {
    bytes: Vec<u8>,
    waiters: VecDeque<SharedWaiter>,
}

impl SharedDataBlock {
    pub(crate) fn new(bytes: Vec<u8>, max_byte_length: Option<usize>) -> Self {
        debug_assert!(max_byte_length.is_none_or(|maximum| bytes.len() <= maximum));
        Self {
            state: Mutex::new(SharedDataBlockState {
                bytes,
                waiters: VecDeque::new(),
            }),
            max_byte_length,
        }
    }

    fn lock(&self) -> MutexGuard<'_, SharedDataBlockState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn byte_length(&self) -> usize {
        self.lock().bytes.len()
    }

    pub(crate) fn max_byte_length(&self) -> usize {
        self.max_byte_length.unwrap_or_else(|| self.byte_length())
    }

    pub(crate) const fn resizable_max_byte_length(&self) -> Option<usize> {
        self.max_byte_length
    }

    pub(crate) const fn is_growable(&self) -> bool {
        self.max_byte_length.is_some()
    }

    pub(crate) fn with_bytes<R>(&self, operation: impl FnOnce(&[u8]) -> R) -> R {
        let state = self.lock();
        operation(&state.bytes)
    }

    pub(crate) fn with_bytes_mut<R>(&self, operation: impl FnOnce(&mut [u8]) -> R) -> R {
        let mut state = self.lock();
        operation(&mut state.bytes)
    }

    /// Grows the shared bytes while holding the Shared Data Block critical
    /// section. Returns `false` when another agent has already grown past the
    /// requested length.
    pub(crate) fn grow(
        &self,
        new_byte_length: usize,
    ) -> Result<bool, std::collections::TryReserveError> {
        let mut state = self.lock();
        if new_byte_length < state.bytes.len() {
            return Ok(false);
        }
        if new_byte_length == state.bytes.len() {
            return Ok(true);
        }
        let additional = new_byte_length - state.bytes.len();
        state.bytes.try_reserve_exact(additional)?;
        state.bytes.resize(new_byte_length, 0);
        Ok(true)
    }

    /// Compares one atomic location and appends its waiter without releasing
    /// the Shared Data Block critical section between those operations.
    pub(crate) fn register_waiter_if_equal(
        &self,
        byte_index: usize,
        expected: &[u8],
        waiter: SharedWaiter,
    ) -> Result<bool, ExecutionError> {
        let mut state = self.lock();
        let Some(end) = byte_index.checked_add(expected.len()) else {
            return Ok(false);
        };
        if state.bytes.get(byte_index..end) != Some(expected) {
            return Ok(false);
        }
        state
            .waiters
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::AtomicsWaiters,
                additional: 1,
            })?;
        state.waiters.push_back(waiter);
        Ok(true)
    }

    /// Runs `create` and appends its waiter only when the compared bytes still
    /// match. The callback executes inside the same Shared Data Block critical
    /// section so a concurrent `notify` cannot pass between the comparison and
    /// registration.
    pub(crate) fn register_waiter_if_equal_with<R>(
        &self,
        byte_index: usize,
        expected: &[u8],
        create: impl FnOnce() -> Result<(SharedWaiter, R), ExecutionError>,
    ) -> Result<Option<R>, ExecutionError> {
        let mut state = self.lock();
        let Some(end) = byte_index.checked_add(expected.len()) else {
            return Ok(None);
        };
        if state.bytes.get(byte_index..end) != Some(expected) {
            return Ok(None);
        }
        state
            .waiters
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::AtomicsWaiters,
                additional: 1,
            })?;
        let (waiter, value) = create()?;
        state.waiters.push_back(waiter);
        Ok(Some(value))
    }

    /// Removes and wakes the first `count` live waiters for one byte location.
    /// Stale timeout/cancellation entries are removed without contributing to
    /// the observable notification count.
    pub(crate) fn notify(
        &self,
        byte_index: usize,
        count: usize,
        notifying_agent: usize,
        direct_token: u64,
    ) -> usize {
        let mut state = self.lock();
        let mut notified = 0_usize;
        let mut cursor = 0_usize;
        while cursor < state.waiters.len() {
            let is_stale = state.waiters[cursor].state.status() != WAITER_PENDING;
            if is_stale {
                let _ = state.waiters.remove(cursor);
                continue;
            }
            if state.waiters[cursor].byte_index != byte_index || notified >= count {
                cursor += 1;
                continue;
            }
            let Some(waiter) = state.waiters.remove(cursor) else {
                break;
            };
            if waiter.state.try_notify() {
                notified = notified.saturating_add(1);
                waiter.wake.notify(waiter.id, notifying_agent, direct_token);
            }
        }
        notified
    }

    pub(crate) fn remove_waiter(&self, byte_index: usize, id: u64) {
        let mut state = self.lock();
        if let Some(index) = state
            .waiters
            .iter()
            .position(|waiter| waiter.byte_index == byte_index && waiter.id == id)
        {
            let _ = state.waiters.remove(index);
        }
    }
}

pub(crate) struct SharedWaiter {
    pub(crate) id: u64,
    pub(crate) byte_index: usize,
    pub(crate) state: Arc<AtomicsWaiterState>,
    pub(crate) wake: SharedWaiterWake,
}

pub(crate) enum SharedWaiterWake {
    Blocking(Arc<BlockingWaiter>),
    Async {
        agent_id: usize,
        sender: UnboundedSender<AtomicsWakeEvent>,
    },
}

impl SharedWaiterWake {
    fn notify(self, waiter_id: u64, notifying_agent: usize, direct_token: u64) {
        match self {
            Self::Blocking(waiter) => waiter.notify(),
            Self::Async { agent_id, sender } => {
                let _ = sender.send(AtomicsWakeEvent {
                    waiter_id,
                    result: AtomicsWakeResult::Ok,
                    direct_token: (agent_id == notifying_agent).then_some(direct_token),
                });
            }
        }
    }
}

pub(crate) struct AtomicsWaiterState {
    status: AtomicU8,
}

impl AtomicsWaiterState {
    pub(crate) const fn pending() -> Self {
        Self {
            status: AtomicU8::new(WAITER_PENDING),
        }
    }

    fn status(&self) -> u8 {
        self.status.load(Ordering::Acquire)
    }

    fn try_notify(&self) -> bool {
        self.status
            .compare_exchange(
                WAITER_PENDING,
                WAITER_NOTIFIED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn try_timeout(&self) -> bool {
        self.status
            .compare_exchange(
                WAITER_PENDING,
                WAITER_TIMED_OUT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn cancel(&self) {
        let _ = self.status.compare_exchange(
            WAITER_PENDING,
            WAITER_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) fn outcome(&self) -> Option<AtomicsWakeResult> {
        match self.status() {
            WAITER_NOTIFIED => Some(AtomicsWakeResult::Ok),
            WAITER_TIMED_OUT => Some(AtomicsWakeResult::TimedOut),
            _ => None,
        }
    }
}

pub(crate) struct BlockingWaiter {
    state: Arc<AtomicsWaiterState>,
    mutex: Mutex<()>,
    condition: Condvar,
}

impl BlockingWaiter {
    pub(crate) fn new(state: Arc<AtomicsWaiterState>) -> Self {
        Self {
            state,
            mutex: Mutex::new(()),
            condition: Condvar::new(),
        }
    }

    fn notify(&self) {
        // Synchronize with the waiter's check-then-wait transition so a
        // notification cannot be lost between the atomic state read and the
        // Condvar sleep.
        let _guard = self
            .mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.condition.notify_one();
    }

    pub(crate) fn wait(&self, timeout: Option<Duration>) -> AtomicsWakeResult {
        let mut guard = self
            .mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
        loop {
            if let Some(outcome) = self.state.outcome() {
                return outcome;
            }
            let Some(deadline) = deadline else {
                guard = self
                    .condition
                    .wait(guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            };
            let now = Instant::now();
            if now >= deadline {
                return if self.state.try_timeout() {
                    AtomicsWakeResult::TimedOut
                } else {
                    self.state.outcome().unwrap_or(AtomicsWakeResult::TimedOut)
                };
            }
            let remaining = deadline.saturating_duration_since(now);
            let waited = self.condition.wait_timeout(guard, remaining);
            let (next_guard, result) = waited.unwrap_or_else(std::sync::PoisonError::into_inner);
            guard = next_guard;
            if result.timed_out() && self.state.try_timeout() {
                return AtomicsWakeResult::TimedOut;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtomicsWakeResult {
    Ok,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AtomicsWakeEvent {
    pub(crate) waiter_id: u64,
    pub(crate) result: AtomicsWakeResult,
    pub(crate) direct_token: Option<u64>,
}
