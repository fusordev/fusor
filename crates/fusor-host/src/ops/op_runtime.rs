//! Async op runtime (§5.5): Tokio-backed spawning with mpsc completion
//! delivery back to the owner task.
//!
//! The single-owner rule is structural: spawned futures carry only
//! `Send + 'static` owned Rust values (enforced by Tokio's spawn bounds at
//! compile time) and never touch engine types. The completion message is a
//! closure capturing the finished `Result<T, OpError>`; it is only *run* on
//! the owner task by [`OpRuntime::poll_completions`], where it serializes
//! the result and settles the paired [`PromiseResolver`].

use std::collections::HashMap;
use std::future::Future;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use fusor_runtime::{Context, PromiseResolver};
use tokio::runtime::Runtime;

use super::{OpError, op_error_value, serialize_value};

/// A completion signal: the pending op id plus the owner-task settlement
/// closure carrying the finished outcome. The two reference parameters have
/// independent anonymous lifetimes (HRTB) so the closure can borrow the
/// resolver for exactly the settlement call.
type Completion = (
    u64,
    Box<dyn FnOnce(&mut Context<'_>, &PromiseResolver) + Send>,
);

/// Error kind for async-op setup failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpRuntimeError {
    /// The Tokio runtime could not be created.
    RuntimeCreation(String),
    /// No [`OpRuntime`] is installed on the owner task.
    NotInstalled,
}

impl std::fmt::Display for OpRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeCreation(message) => {
                write!(formatter, "op runtime creation failed: {message}")
            }
            Self::NotInstalled => formatter.write_str(
                "no op runtime is installed on the owner task (call install_op_runtime first)",
            ),
        }
    }
}

impl std::error::Error for OpRuntimeError {}

/// Owns the Tokio worker runtime and the completion channel for async ops.
///
/// One instance lives on the owner task; spawned futures run on Tokio
/// worker threads and only send their settlement closure back over the
/// channel. The owner task polls [`Self::poll_completions`] to run the
/// closure and settle the paired promise.
#[derive(Debug)]
pub struct OpRuntime {
    runtime: Runtime,
    sender: Sender<Completion>,
    receiver: Receiver<Completion>,
    pending: HashMap<u64, PromiseResolver>,
    next_id: u64,
}

impl OpRuntime {
    /// Creates the worker runtime and the completion channel.
    ///
    /// # Errors
    ///
    /// Returns [`OpRuntimeError::RuntimeCreation`] when Tokio cannot start.
    pub fn new() -> Result<Self, OpRuntimeError> {
        let runtime = Runtime::new()
            .map_err(|error| OpRuntimeError::RuntimeCreation(error.to_string()))?;
        let (sender, receiver) = mpsc::channel();
        Ok(Self {
            runtime,
            sender,
            receiver,
            pending: HashMap::new(),
            next_id: 0,
        })
    }

    /// Spawns one async op future and pairs its completion with `resolver`.
    ///
    /// The future is `Send + 'static` and captures only owned Rust values,
    /// so the engine heap is never touched off the owner task (§5.5). When
    /// the future finishes, its settlement closure is sent over the
    /// channel; [`Self::poll_completions`] runs it on the owner task.
    ///
    /// # Errors
    ///
    /// Returns an [`OpRuntimeError`] when the future cannot be spawned.
    pub fn spawn<T, F>(&mut self, resolver: PromiseResolver, future: F) -> Result<(), OpRuntimeError>
    where
        T: serde::Serialize + Send + 'static,
        F: Future<Output = Result<T, OpError>> + Send + 'static,
    {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let sender = self.sender.clone();
        self.pending.insert(id, resolver);
        self.runtime.spawn(async move {
            let outcome = future.await;
            let settlement = move |context: &mut Context<'_>, resolver: &PromiseResolver| {
                settle_pending::<T>(context, resolver, outcome);
            };
            let _ = sender.send((id, Box::new(settlement)));
        });
        Ok(())
    }

    /// Drains finished op completions on the owner task, running each
    /// settlement closure (serialization plus promise settlement) here.
    ///
    /// Returns the number of completions settled in this poll.
    pub fn poll_completions(&mut self, context: &mut Context<'_>) -> usize {
        let mut settled = 0;
        loop {
            match self.receiver.try_recv() {
                Ok((id, settlement)) => {
                    if let Some(resolver) = self.pending.remove(&id) {
                        settlement(context, &resolver);
                        settled += 1;
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        settled
    }

    /// Returns the number of op futures still running.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// Installs the owner-task [`OpRuntime`] used by `#[op(async)]` glue.
///
/// Installation is a one-time host bootstrap step.
///
/// # Errors
///
/// Returns the runtime unchanged when one is already installed.
pub fn install_op_runtime(runtime: OpRuntime) -> Result<(), OpRuntime> {
    OP_RUNTIME.with(|slot| {
        if slot.borrow().is_some() {
            return Err(runtime);
        }
        *slot.borrow_mut() = Some(runtime);
        Ok(())
    })
}

/// Spawns one async op future through the installed owner-task runtime.
///
/// # Errors
///
/// Returns [`OpRuntimeError::NotInstalled`] when no runtime is installed
/// (fail closed: async ops need the host bootstrap).
pub fn spawn_op<T, F>(resolver: PromiseResolver, future: F) -> Result<(), OpRuntimeError>
where
    T: serde::Serialize + Send + 'static,
    F: Future<Output = Result<T, OpError>> + Send + 'static,
{
    OP_RUNTIME.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .ok_or(OpRuntimeError::NotInstalled)?
            .spawn(resolver, future)
    })
}

/// Polls the installed owner-task runtime's completion channel, settling
/// every finished async op's promise (the event-loop integration point of
/// §5.5).
///
/// # Errors
///
/// Returns [`OpRuntimeError::NotInstalled`] when no runtime is installed.
pub fn poll_op_completions(context: &mut Context<'_>) -> Result<usize, OpRuntimeError> {
    OP_RUNTIME.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .ok_or(OpRuntimeError::NotInstalled)
            .map(|runtime| runtime.poll_completions(context))
    })
}

thread_local! {
    static OP_RUNTIME: std::cell::RefCell<Option<OpRuntime>> =
        const { std::cell::RefCell::new(None) };
}

/// Settles one finished op promise with its stored outcome.
///
/// Runs on the owner task only: serializes the result through the serde
/// bridge and resolves, or converts the [`OpError`] into the rejection
/// value (§5.3, §5.5).
fn settle_pending<T: serde::Serialize>(
    context: &mut Context<'_>,
    resolver: &PromiseResolver,
    outcome: Result<T, OpError>,
) {
    match outcome {
        Ok(value) => match serialize_value(context, &value) {
            Ok(serialized) => {
                let _ = resolver.resolve(context, serialized);
            }
            Err(thrown) => {
                let _ = resolver.reject(context, thrown);
            }
        },
        Err(error) => {
            let rejection = op_error_value(context, error);
            let _ = resolver.reject(context, rejection);
        }
    }
}
