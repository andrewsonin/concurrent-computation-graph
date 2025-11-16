use crate::{
    config::Config,
    task::{SetupApi, TaskSubscriptionId},
    types::{IndexMap, IndexSet, TaskConfig, TaskId},
};
use thiserror::Error;

/// Error kind for executor setup failures.
///
/// Currently indicates that the provided dependency graph contains cycle(s),
/// making it impossible to derive a valid topological ordering.
#[derive(Debug, Error, Clone, PartialEq)]
#[non_exhaustive]
pub enum ExecutorSetupError {
    /// The provided dependency graph contains cycle(s).
    #[error("graph contains cycle(s)")]
    Cycle,
}

pub(super) struct TaskSetupApiImpl<'a, C: Config> {
    pub(super) current_task_id: TaskId,
    pub(super) task_configs: &'a IndexMap<TaskId, TaskConfig<C>>,
    pub(super) child_to_parents: &'a mut IndexMap<TaskId, IndexSet<TaskId>>,
    pub(super) parent_to_children: &'a mut IndexMap<TaskId, IndexSet<TaskId>>,
}

impl<C: Config> SetupApi for TaskSetupApiImpl<'_, C> {
    fn subscribe(&mut self, parent_task_id: TaskId) -> TaskSubscriptionId {
        let Self {
            current_task_id,
            task_configs,
            child_to_parents,
            parent_to_children,
        } = self;
        assert!(
            task_configs.contains_key(&parent_task_id),
            "Task {parent_task_id:?} is subscribed to but its config is missing"
        );
        assert_ne!(
            *current_task_id, parent_task_id,
            "Task {parent_task_id:?} is subscribed to itself"
        );
        child_to_parents
            .entry(*current_task_id)
            .or_default()
            .insert(parent_task_id);
        parent_to_children
            .entry(parent_task_id)
            .or_default()
            .insert(*current_task_id);

        TaskSubscriptionId(parent_task_id)
    }
}
