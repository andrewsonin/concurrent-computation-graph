use crate::{
    config::Config,
    types::{TaskId, TaskOutput},
};
use core::fmt::Debug;

/// A single unit of computation in the DAG.
///
/// Tasks are constructed once during `Executor::setup` via `Task::setup`, where
/// they may declare dependencies on other tasks using the provided `SetupApi`.
/// At runtime, `exec` is invoked when all parents have completed, and the task
/// may read parents' outputs via the `ExecApi`.
pub trait Task<C: Config>: Debug + Send + Sync {
    /// Task configuration type.
    type Config;
    /// Task output type.
    type Output: Send + Sync;
    /// Construct the task from a user-provided config, optionally subscribing
    /// to parents via `api.subscribe(parent_id)`.
    fn setup(api: &mut impl SetupApi, config: &Self::Config) -> Self;
    /// Execute the task and return its output.
    ///
    /// The task may call `data.read(subscription_id)` to access the output
    /// of a previously subscribed parent.
    fn exec(&mut self, data: &impl ExecApi<C>) -> Self::Output;
}

/// API available to tasks during construction to declare dependencies
/// (subscriptions).
pub trait SetupApi {
    /// Subscribe to a parent task by its `TaskId` and receive a stable
    /// subscription token that can later be used to read the parent's
    /// output during `exec`.
    fn subscribe(&mut self, parent_task_id: TaskId) -> TaskSubscriptionId;
}

/// API available to tasks at execution time for reading parents' outputs.
pub trait ExecApi<C: Config> {
    /// Read the output of a parent previously subscribed to via `SetupApi`.
    ///
    /// Safety is guaranteed by the executor: this is only called after the
    /// corresponding parent has completed and published its output.
    fn read(&self, subscription_id: TaskSubscriptionId) -> &TaskOutput<C>;
}

/// Opaque handle representing a subscription to a parent task's output.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct TaskSubscriptionId(pub(crate) TaskId);
