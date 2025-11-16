use crate::{
    config::Config,
    executor::{OutputSlot, TaskLayout, TaskSlot},
    sync::{Ordering, fence},
    task::{ExecApi, Task, TaskSubscriptionId},
    types::{HashMap, TaskId, TaskOutput},
    utils::RangeSplitAtHalf,
};
use std::ops::Range;

pub(super) struct TaskExecApiImpl<'a, C: Config> {
    pub(super) outputs: &'a [OutputSlot<C>],
    pub(super) task_id_to_index: &'a HashMap<TaskId, u16>,
}

impl<C: Config> Clone for TaskExecApiImpl<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C: Config> Copy for TaskExecApiImpl<'_, C> {}

impl<C: Config> ExecApi<C> for TaskExecApiImpl<'_, C> {
    fn read(&self, subscription_id: TaskSubscriptionId) -> &TaskOutput<C> {
        let Self {
            outputs,
            task_id_to_index,
        } = self;
        let TaskSubscriptionId(id) = subscription_id;
        let task_index = task_id_to_index[&id];
        // SAFETY:
        // - `tasks[task_index]` is already executed, so there are no concurrent writes
        //   to it.
        // - `tasks[task_index].output` is initialized.
        unsafe {
            let task = &outputs[task_index as usize];
            task.get().as_ref().unwrap().as_ref().unwrap()
        }
    }
}

pub(super) unsafe fn join_independent_tasks<C: Config>(
    tasks: &[TaskSlot<C>],
    outputs: &[OutputSlot<C>],
    independent_task_range: Range<usize>,
    exec_api: TaskExecApiImpl<'_, C>,
) {
    match independent_task_range.len() {
        0 => return,
        1 => {
            let task_slot = tasks[independent_task_range.clone()].first().unwrap();
            let output_slot = outputs[independent_task_range].first().unwrap();
            unsafe {
                task_slot
                    .get()
                    .as_mut()
                    .unwrap()
                    .exec(exec_api, tasks, outputs, output_slot);
            }
            return;
        }
        _ => {}
    }

    let (left_range, right_range) = independent_task_range.split_at_half();
    unsafe {
        rayon::join(
            || join_independent_tasks(tasks, outputs, left_range, exec_api),
            || join_independent_tasks(tasks, outputs, right_range, exec_api),
        );
    }
}

impl<C: Config> TaskLayout<C> {
    unsafe fn exec(
        &mut self,
        exec_api: TaskExecApiImpl<'_, C>,
        tasks: &[TaskSlot<C>],
        outputs: &[OutputSlot<C>],
        output: &OutputSlot<C>,
    ) {
        let Self {
            task,
            owned_children,
            shared_children,
            parents_total,
            parents_left,
        } = self;
        unsafe {
            *output.get().as_mut().unwrap() = Some(task.exec(&exec_api));
        }

        let num_owned_tasks = owned_children.len();

        if !shared_children.is_empty() {
            fence(Ordering::Release);
        }
        for &task_index in shared_children.iter() {
            let shared_task = unsafe { &*tasks[task_index as usize].get() };
            if shared_task.parents_left.fetch_sub(1, Ordering::Relaxed) == 1 {
                owned_children.push(task_index);
            }
        }
        if owned_children.len() != num_owned_tasks {
            fence(Ordering::Acquire);
        }

        unsafe { join_owned_tasks(tasks, outputs, owned_children, exec_api) };

        *parents_left.get_mut() = *parents_total;
        owned_children.resize_with(num_owned_tasks, || unreachable!("TaskLayout::exec"));
    }
}

unsafe fn join_owned_tasks<C: Config>(
    tasks: &[TaskSlot<C>],
    outputs: &[OutputSlot<C>],
    owned_task_indexes: &[u16],
    exec_api: TaskExecApiImpl<'_, C>,
) {
    match owned_task_indexes.first() {
        None => return,
        Some(&task_index) if owned_task_indexes.len() == 1 => {
            unsafe {
                let task_layout = &mut *tasks[task_index as usize].get();
                let output = &outputs[task_index as usize];
                task_layout.exec(exec_api, tasks, outputs, output);
            }
            return;
        }
        _ => {}
    }
    let (left, right) = owned_task_indexes.split_at(owned_task_indexes.len() / 2);
    unsafe {
        rayon::join(
            || join_owned_tasks(tasks, outputs, left, exec_api),
            || join_owned_tasks(tasks, outputs, right, exec_api),
        );
    }
}
