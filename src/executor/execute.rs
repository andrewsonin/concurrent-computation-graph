use crate::{
    config::Config,
    executor::{OutputSlot, ParentInfoSlot, TaskLayout},
    sync::*,
    task::{ExecApi, Task, TaskSubscriptionId},
    types::{HashMap, TaskId, TaskOutput},
    utils::RangeSplitAtHalf,
};
use core::{
    marker::PhantomData,
    ops::{Deref, Range},
};

#[cfg(not(feature = "loom"))]
pub(super) struct TaskExecApiImpl<'a, C: Config> {
    pub(super) outputs: OutputSlots<'a, C>,
    pub(super) task_id_to_index: &'a HashMap<TaskId, u16>,
    pub(super) _marker: PhantomData<&'a ()>,
}

#[cfg(feature = "loom")]
pub(super) struct TaskExecApiImpl<'a: 'static, C: Config> {
    pub(super) outputs: OutputSlots<'a, C>,
    pub(super) task_id_to_index: std::sync::Arc<HashMap<TaskId, u16>>,
    pub(super) _marker: PhantomData<&'a ()>,
}

impl<C: Config> Clone for TaskExecApiImpl<'_, C> {
    fn clone(&self) -> Self {
        let Self {
            outputs,
            task_id_to_index,
            _marker,
        } = self;
        // Just a zero-cost hack for `loom` to work.
        #[allow(suspicious_double_ref_op)]
        Self {
            outputs: outputs.clone(),
            task_id_to_index: task_id_to_index.clone(),
            _marker: PhantomData,
        }
    }
}

impl<C: Config> ExecApi<C> for TaskExecApiImpl<'_, C> {
    fn read(&self, subscription_id: TaskSubscriptionId) -> &TaskOutput<C> {
        let Self {
            outputs,
            task_id_to_index,
            _marker,
        } = self;
        let TaskSubscriptionId(id) = subscription_id;
        let task_index = task_id_to_index[&id];
        // SAFETY:
        // - `tasks[task_index]` is already executed, so there are no concurrent writes
        //   to the corresponding output slot `outputs[task_index]`.
        // - `outputs[task_index]` is initialized.
        unsafe {
            let task = &outputs[task_index as usize];
            task.get()
                .with(|ptr| ptr.as_ref().unwrap().as_ref().unwrap())
        }
    }
}

/// # Safety
///
/// - `tasks`, `outputs`, and `parents` must correspond to the same execution
///   plan and have identical lengths.
/// - `independent_task_range` must be within bounds of those slices and contain
///   only independent tasks (no parents), so each can be started immediately.
/// - The executor must ensure each task index is executed at most once along
///   any path, preventing aliasing-related UB when using interior mutability.
pub(super) unsafe fn join_independent_tasks<C: Config>(
    tasks: TaskSlots<'_, C>,
    outputs: OutputSlots<'_, C>,
    parents: ParentInfoSlots<'_>,
    independent_task_range: Range<usize>,
    exec_api: TaskExecApiImpl<'_, C>,
) {
    match independent_task_range.len() {
        0 => return,
        1 => {
            let idx = independent_task_range.start;
            let task = &tasks[idx];
            let output = &outputs[idx];
            let parent = &parents[idx];
            #[cfg(feature = "loom")]
            let outputs = outputs.clone();
            #[cfg(feature = "loom")]
            let parents = parents.clone();
            // SAFETY: `idx` is in range; scheduling guarantees this task is ready to
            // execute and no concurrent mutable access to its slot/output
            // occurs during the call.
            unsafe {
                task.get_mut().with(|ptr| {
                    ptr.as_mut()
                        .unwrap()
                        .exec(exec_api, tasks, outputs, parents, output, parent);
                });
            }
            return;
        }
        _ => {}
    }

    let (left_range, right_range) = independent_task_range.split_at_half();
    // Just a zero-cost hack for `loom` to work.
    #[allow(noop_method_call)]
    let (rhs_tasks, rhs_outputs, rhs_parents, rhs_exec_api) = (
        tasks.clone(),
        outputs.clone(),
        parents.clone(),
        exec_api.clone(),
    );
    // SAFETY: The two closures operate on disjoint index ranges (`left_range`,
    // `right_range`). Shared slices are accessed at non-overlapping indices, so
    // no data races occur.
    unsafe {
        join(
            || join_independent_tasks(tasks, outputs, parents, left_range, exec_api),
            || {
                join_independent_tasks(
                    rhs_tasks,
                    rhs_outputs,
                    rhs_parents,
                    right_range,
                    rhs_exec_api,
                );
            },
        );
    }
}

impl<C: Config> TaskLayout<C> {
    /// # Safety
    ///
    /// - All parents of this task have completed and published outputs before
    ///   this call.
    /// - `tasks`, `outputs`, and `parents` are aligned slices for the same
    ///   plan; `output` is this task's output slot and is written exactly once
    ///   per execution.
    /// - `parent` refers to this task's own parent-info slot used for counter
    ///   reset.
    /// - Any indices appended to `owned_children` are valid and ready to
    ///   execute in this thread (owned edges or last shared-parent observed
    ///   here).
    unsafe fn exec(
        &mut self,
        exec_api: TaskExecApiImpl<'_, C>,
        tasks: TaskSlots<'_, C>,
        outputs: OutputSlots<'_, C>,
        parents: ParentInfoSlots<'_>,
        output: &OutputSlot<C>,
        parent: &ParentInfoSlot,
    ) {
        let Self {
            task,
            owned_children,
            shared_children,
        } = self;
        // SAFETY: We have exclusive logical access to this task's output slot; it is
        // uninitialized prior to this store and will be read only after publication.
        unsafe {
            output
                .get_mut()
                .with(|ptr| *ptr.as_mut().unwrap() = Some(task.exec(&exec_api)));
        }

        let num_owned_tasks = owned_children.len();

        if !shared_children.is_empty() {
            fence(Ordering::Release);
        }
        for &task_index in shared_children.iter() {
            let parent_info = &parents[task_index as usize];
            if parent_info.parents_left.fetch_sub(1, Ordering::Relaxed) == 1 {
                owned_children.push(task_index);
            }
        }
        if owned_children.len() != num_owned_tasks {
            fence(Ordering::Acquire);
        }

        // SAFETY: All indices in `owned_children` are ready to run here, and an Acquire
        // fence was issued if any were promoted from shared. Slices refer to the same
        // plan.
        unsafe { join_owned_tasks(tasks, outputs, parents, owned_children.as_slice(), exec_api) };

        owned_children.resize_with(num_owned_tasks, || unreachable!("TaskLayout::exec"));
        parent
            .parents_left
            .store(parent.parents_total, Ordering::Relaxed);
    }
}

/// # Safety
///
/// - `owned_task_indexes` must contain valid task indices ready to execute in
///   the current thread (either owned edges or shared children whose counters
///   reached zero here).
/// - `tasks`, `outputs`, and `parents` are aligned slices for the same plan.
/// - Each task is executed at most once per invocation, avoiding aliasing
///   violations.
unsafe fn join_owned_tasks<C: Config>(
    tasks: TaskSlots<'_, C>,
    outputs: OutputSlots<'_, C>,
    parents: ParentInfoSlots<'_>,
    owned_task_indexes: impl Deref<Target = [u16]>,
    exec_api: TaskExecApiImpl<'_, C>,
) {
    match owned_task_indexes.first() {
        None => return,
        Some(&task_index) if owned_task_indexes.len() == 1 => {
            let idx = task_index as usize;
            let task = &tasks[idx];
            let output = &outputs[idx];
            let parent = &parents[idx];
            #[cfg(feature = "loom")]
            let outputs = outputs.clone();
            #[cfg(feature = "loom")]
            let parents = parents.clone();
            // SAFETY: `idx` is valid; this task is ready to execute with no concurrent
            // mutable access to its slot/output during the call.
            unsafe {
                task.get_mut().with(|ptr| {
                    ptr.as_mut()
                        .unwrap()
                        .exec(exec_api, tasks, outputs, parents, output, parent);
                });
            }
            return;
        }
        _ => {}
    }
    let (left, right) = owned_task_indexes.split_at(owned_task_indexes.len() / 2);
    #[cfg(feature = "loom")]
    let (left, right) = (std::sync::Arc::from(left), std::sync::Arc::from(right));
    // Just a zero-cost hack for `loom` to work.
    #[allow(noop_method_call)]
    let (rhs_tasks, rhs_outputs, rhs_parents, rhs_exec_api) = (
        tasks.clone(),
        outputs.clone(),
        parents.clone(),
        exec_api.clone(),
    );
    // SAFETY: The two closures operate on disjoint sets of task indices (`left`,
    // `right`). Shared slices are accessed at non-overlapping indices,
    // preventing data races.
    unsafe {
        join(
            || join_owned_tasks(tasks, outputs, parents, left, exec_api),
            || join_owned_tasks(rhs_tasks, rhs_outputs, rhs_parents, right, rhs_exec_api),
        );
    }
}
