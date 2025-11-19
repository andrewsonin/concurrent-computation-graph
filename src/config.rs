use crate::task::Task;

/// Configuration entry-point for instantiating the executor.
///
/// A concrete `Config` binds a specific `Task` implementation to the executor,
/// defining both the task configuration type and the task output type via
/// the associated types on `Task`.
pub trait Config: Sized + 'static {
    /// The user-defined task type that the executor will construct and run.
    type Task: Task<Self>;
}
