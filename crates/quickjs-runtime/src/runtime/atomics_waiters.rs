//! Runtime-owned async Atomics waiter roots and Tokio deadline signaling.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    thread::JoinHandle,
    time::Duration,
};

use tokio::{
    runtime::Builder as TokioRuntimeBuilder,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    time::Instant,
};

use super::{ObjectId, RealmId, Runtime, RuntimeResource, check_execution_limit, usize_to_u64};
use crate::{
    EngineFault, ExecutionError, JsString,
    shared_array_buffer::{
        AtomicsWaiterState, AtomicsWakeEvent, AtomicsWakeResult, SharedDataBlock, SharedWaiter,
        SharedWaiterWake, next_atomics_waiter_id,
    },
    value::StoredValue,
};

pub(crate) struct AsyncAtomicsWaiter {
    pub(crate) block: Arc<SharedDataBlock>,
    pub(crate) byte_index: usize,
    pub(crate) promise: ObjectId,
    pub(crate) state: Arc<AtomicsWaiterState>,
    has_timer: bool,
}

enum AtomicsTimerCommand {
    Schedule {
        waiter_id: u64,
        deadline: Instant,
        state: Arc<AtomicsWaiterState>,
    },
    Cancel {
        waiter_id: u64,
    },
    Shutdown,
}

pub(crate) struct AtomicsTimerDriver {
    commands: UnboundedSender<AtomicsTimerCommand>,
    thread: Option<JoinHandle<()>>,
}

impl AtomicsTimerDriver {
    fn start(wake: UnboundedSender<AtomicsWakeEvent>) -> Result<Self, ExecutionError> {
        let timer = TokioRuntimeBuilder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::AtomicsWaiters,
                additional: 1,
            })?;
        let (commands, receiver) = mpsc::unbounded_channel();
        let thread = std::thread::Builder::new()
            .name("quickjs-atomics-timer".to_owned())
            .spawn(move || timer.block_on(run_timer_driver(receiver, wake)))
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::AtomicsWaiters,
                additional: 1,
            })?;
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    fn schedule(
        &self,
        waiter_id: u64,
        delay: Duration,
        state: Arc<AtomicsWaiterState>,
    ) -> Result<(), ExecutionError> {
        let Some(deadline) = Instant::now().checked_add(delay) else {
            return Ok(());
        };
        self.commands
            .send(AtomicsTimerCommand::Schedule {
                waiter_id,
                deadline,
                state,
            })
            .map_err(|_| atomics_timer_stopped())
    }

    fn cancel(&self, waiter_id: u64) {
        let _ = self
            .commands
            .send(AtomicsTimerCommand::Cancel { waiter_id });
    }
}

impl Drop for AtomicsTimerDriver {
    fn drop(&mut self) {
        let _ = self.commands.send(AtomicsTimerCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn run_timer_driver(
    mut commands: UnboundedReceiver<AtomicsTimerCommand>,
    wake: UnboundedSender<AtomicsWakeEvent>,
) {
    let mut deadlines: BTreeMap<(Instant, u64), Arc<AtomicsWaiterState>> = BTreeMap::new();
    let mut waiter_deadlines: HashMap<u64, Instant> = HashMap::new();
    loop {
        let next = deadlines.first_key_value().map(|(key, _)| *key);
        let command = if let Some((deadline, _)) = next {
            if let Ok(command) = tokio::time::timeout_at(deadline, commands.recv()).await {
                command
            } else {
                expire_ready_waiters(&mut deadlines, &mut waiter_deadlines, &wake, Instant::now());
                continue;
            }
        } else {
            commands.recv().await
        };
        let Some(command) = command else {
            break;
        };
        match command {
            AtomicsTimerCommand::Schedule {
                waiter_id,
                deadline,
                state,
            } => {
                if let Some(previous) = waiter_deadlines.insert(waiter_id, deadline) {
                    deadlines.remove(&(previous, waiter_id));
                }
                deadlines.insert((deadline, waiter_id), state);
            }
            AtomicsTimerCommand::Cancel { waiter_id } => {
                if let Some(deadline) = waiter_deadlines.remove(&waiter_id) {
                    deadlines.remove(&(deadline, waiter_id));
                }
            }
            AtomicsTimerCommand::Shutdown => break,
        }
    }
}

fn expire_ready_waiters(
    deadlines: &mut BTreeMap<(Instant, u64), Arc<AtomicsWaiterState>>,
    waiter_deadlines: &mut HashMap<u64, Instant>,
    wake: &UnboundedSender<AtomicsWakeEvent>,
    now: Instant,
) {
    while deadlines
        .first_key_value()
        .is_some_and(|((deadline, _), _)| *deadline <= now)
    {
        let Some(((deadline, waiter_id), state)) = deadlines.pop_first() else {
            break;
        };
        if waiter_deadlines.get(&waiter_id) != Some(&deadline) {
            continue;
        }
        waiter_deadlines.remove(&waiter_id);
        if state.try_timeout() {
            let _ = wake.send(AtomicsWakeEvent {
                waiter_id,
                result: AtomicsWakeResult::TimedOut,
                direct_token: None,
            });
        }
    }
}

const fn atomics_timer_stopped() -> ExecutionError {
    ExecutionError::EngineFault(EngineFault::RuntimeInvariant {
        message: "Atomics timeout driver stopped unexpectedly",
    })
}

impl Runtime {
    pub(crate) fn register_async_atomics_waiter(
        &mut self,
        block: &Arc<SharedDataBlock>,
        byte_index: usize,
        expected: &[u8],
        realm: RealmId,
        timeout: Option<Duration>,
    ) -> Result<Option<(u64, ObjectId)>, ExecutionError> {
        let wake = self.atomics_wake_sender.clone();
        let block_for_record = Arc::clone(block);
        let registration = block.register_waiter_if_equal_with(byte_index, expected, || {
            check_execution_limit(
                RuntimeResource::AtomicsWaiters,
                self.limits.max_pending_atomics_waiters,
                usize_to_u64(self.atomics_waiters.len()).saturating_add(1),
            )?;
            self.atomics_waiters
                .try_reserve(1)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::AtomicsWaiters,
                    additional: 1,
                })?;
            let required_ready_capacity = self.atomics_waiters.len().saturating_add(1);
            let additional_ready_capacity =
                required_ready_capacity.saturating_sub(self.atomics_ready.len());
            self.atomics_ready
                .try_reserve(additional_ready_capacity)
                .map_err(|_| ExecutionError::AllocationFailed {
                    resource: RuntimeResource::AtomicsWaiters,
                    additional: additional_ready_capacity,
                })?;
            let promise = self.allocate_intrinsic_promise(realm)?;
            let waiter_id = next_atomics_waiter_id();
            let state = Arc::new(AtomicsWaiterState::pending());
            let previous = self.atomics_waiters.insert(
                waiter_id,
                AsyncAtomicsWaiter {
                    block: Arc::clone(&block_for_record),
                    byte_index,
                    promise,
                    state: Arc::clone(&state),
                    has_timer: timeout.is_some(),
                },
            );
            debug_assert!(previous.is_none());
            Ok::<_, ExecutionError>((
                SharedWaiter {
                    id: waiter_id,
                    byte_index,
                    state: Arc::clone(&state),
                    wake: SharedWaiterWake::Async {
                        agent_id: self.atomics_agent_id,
                        sender: wake,
                    },
                },
                (waiter_id, promise, state),
            ))
        })?;
        let Some((waiter_id, promise, state)) = registration else {
            return Ok(None);
        };
        if let Some(delay) = timeout
            && let Err(error) = self.schedule_atomics_timeout(waiter_id, delay, state)
        {
            self.cancel_atomics_waiter(waiter_id);
            return Err(error);
        }
        self.collection_pending = true;
        Ok(Some((waiter_id, promise)))
    }

    fn schedule_atomics_timeout(
        &mut self,
        waiter_id: u64,
        delay: Duration,
        state: Arc<AtomicsWaiterState>,
    ) -> Result<(), ExecutionError> {
        if self.atomics_timer.is_none() {
            self.atomics_timer = Some(AtomicsTimerDriver::start(self.atomics_wake_sender.clone())?);
        }
        self.atomics_timer
            .as_ref()
            .ok_or_else(atomics_timer_stopped)?
            .schedule(waiter_id, delay, state)
    }

    pub(crate) fn settle_next_ready_atomics_waiter(&mut self) -> Result<bool, ExecutionError> {
        while let Ok(event) = self.atomics_wake_receiver.try_recv() {
            self.queue_ready_atomics_event(event)?;
        }

        while let Some(event) = self.atomics_ready.front().copied() {
            let did_settle = self.settle_atomics_wake_event(event)?;
            self.atomics_ready.pop_front();
            if did_settle {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn settle_notified_atomics_waiters(
        &mut self,
        direct_token: u64,
    ) -> Result<usize, ExecutionError> {
        let mut settled = 0_usize;
        while let Ok(event) = self.atomics_wake_receiver.try_recv() {
            if event.direct_token == Some(direct_token) {
                match self.settle_atomics_wake_event(event) {
                    Ok(true) => settled = settled.saturating_add(1),
                    Ok(false) => {}
                    Err(error) => {
                        self.queue_ready_atomics_event(event)?;
                        return Err(error);
                    }
                }
            } else {
                self.queue_ready_atomics_event(event)?;
            }
        }
        Ok(settled)
    }

    fn queue_ready_atomics_event(&mut self, event: AtomicsWakeEvent) -> Result<(), ExecutionError> {
        self.atomics_ready
            .try_reserve(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::AtomicsWaiters,
                additional: 1,
            })?;
        self.atomics_ready.push_back(event);
        Ok(())
    }

    fn settle_atomics_wake_event(
        &mut self,
        event: AtomicsWakeEvent,
    ) -> Result<bool, ExecutionError> {
        let Some(waiter) = self.atomics_waiters.get(&event.waiter_id) else {
            return Ok(false);
        };
        if waiter.state.outcome() != Some(event.result) {
            return Ok(false);
        }
        let promise = waiter.promise;
        let block = Arc::clone(&waiter.block);
        let byte_index = waiter.byte_index;
        let has_timer = waiter.has_timer;
        block.remove_waiter(byte_index, event.waiter_id);
        crate::vm::fulfill_promise_host(
            self,
            promise,
            StoredValue::String(JsString::from_utf8(match event.result {
                AtomicsWakeResult::Ok => "ok",
                AtomicsWakeResult::TimedOut => "timed-out",
            })?),
        )?;
        self.atomics_waiters.remove(&event.waiter_id);
        if has_timer && let Some(timer) = &self.atomics_timer {
            timer.cancel(event.waiter_id);
        }
        Ok(true)
    }

    pub(crate) fn cancel_atomics_waiter(&mut self, waiter_id: u64) {
        let Some(waiter) = self.atomics_waiters.remove(&waiter_id) else {
            return;
        };
        waiter.state.cancel();
        waiter.block.remove_waiter(waiter.byte_index, waiter_id);
        if waiter.has_timer
            && let Some(timer) = &self.atomics_timer
        {
            timer.cancel(waiter_id);
        }
    }

    fn cancel_all_atomics_waiters(&mut self) {
        let waiter_ids: Vec<u64> = self.atomics_waiters.keys().copied().collect();
        for waiter_id in waiter_ids {
            self.cancel_atomics_waiter(waiter_id);
        }
        self.atomics_ready.clear();
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.cancel_all_atomics_waiters();
        self.atomics_timer.take();
    }
}
