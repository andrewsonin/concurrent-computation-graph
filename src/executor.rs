mod execute;
mod setup;

/// Error returned by `Executor::setup` when the provided task graph is invalid.
///
/// Specifically, this is produced when cycle detection determines that
/// no valid topological order exists for part or all of the graph.
pub use crate::executor::setup::ExecutorSetupError;
use crate::{
    config::Config,
    executor::{
        execute::{TaskExecApiImpl, join_independent_tasks},
        setup::TaskSetupApiImpl,
    },
    sync::AtomicU16,
    task::Task,
    types::{HashMap, HashSet, IndexMap, IndexSet, SyncUnsafeCell, TaskConfig, TaskId, TaskOutput},
};
use core::marker::PhantomData;
use derive_more::Debug;
use rustc_hash::FxBuildHasher;
use std::collections::VecDeque;
use unzip3::Unzip3;

/// Concurrent DAG executor that builds a topological plan and runs tasks in
/// parallel.
///
/// Key responsibilities:
/// - Validates the dependency graph and detects cycles during `setup`.
/// - Computes a stable topological order and maps each `TaskId` to an index.
/// - Splits children into `owned` (single parent) and `shared` (multiple
///   parents) to minimize synchronization during `execute`.
/// - Executes independent roots in parallel and propagates readiness downstream
///   using a fence+counter protocol for shared children.
#[must_use]
#[derive(Debug)]
pub struct Executor<C: Config> {
    tasks: Vec<TaskSlot<C>>,
    outputs: Vec<OutputSlot<C>>,
    parents: Vec<ParentInfoSlot>,
    num_independent_tasks: u16,
    task_id_to_index: HashMap<TaskId, u16>,
}

pub(crate) type TaskSlot<C> = SyncUnsafeCell<TaskLayout<C>>;

#[must_use]
#[derive(Debug)]
pub(crate) struct TaskLayout<C: Config> {
    /// Current task.
    task: C::Task,
    /// Indexes of the downstream tasks that are dependent on the current task
    /// and don't have other parents. That is, they can be started
    /// immediately after the current task is completed without any
    /// multithread synchronization.
    owned_children: Vec<u16>,
    /// Indexes of the downstream tasks that are dependent on the current task
    /// but have multiple parents.
    shared_children: Vec<u16>,
}

#[must_use]
#[derive(Debug)]
#[repr(align(128))]
pub(crate) struct ParentInfoSlot {
    /// Total number of parent tasks.
    parents_total: u16,
    /// Number of parent tasks that haven't finished yet.
    parents_left: AtomicU16,
}

pub(crate) type OutputSlot<C> = SyncUnsafeCell<Option<TaskOutput<C>>>;

struct TaskSetupLayout<'a> {
    task_idx: u16,
    degree: u16,
    parents: Option<&'a IndexSet<TaskId>>,
    children: Option<&'a IndexSet<TaskId>>,
}

impl<C: Config> Executor<C> {
    /// Construct an executor by:
    /// - Building the dependency maps via `Task::setup` subscriptions,
    /// - Seeding roots (independent tasks),
    /// - Performing Kahn-like processing to topologically order tasks with
    ///   robust cycle detection,
    /// - Materializing per-task scheduling metadata and indices.
    ///
    /// # Panics
    /// In case of internal invariant violations. Impossible if there are no
    /// bugs in the code.
    ///
    /// # Errors
    /// In case if the graph contains cycle(s).
    #[allow(clippy::too_many_lines)]
    pub fn setup(
        task_configs: &IndexMap<TaskId, TaskConfig<C>>,
    ) -> Result<Self, ExecutorSetupError> {
        // Example (intended invariants on order/degree/children classification):
        //
        //   A     B
        //    \   /
        //      C
        //      |
        //      D
        //
        // - Roots: A, B (no parents) => appear first in the final order; degree(A) = 0,
        //   degree(B) = 0.
        // - C depends on both A and B => degree(C) = 1 + max(degree(A), degree(B)) = 1.
        // - D depends on C only       => degree(D) = 1 + degree(C) = 2.
        // - owned_children: an edge parent->child is "owned" iff the child has exactly
        //   one parent. In the diagram: C has two parents => edges A->C and B->C are
        //   shared (not owned). D has one parent (C), so C->D is owned.
        // - shared_children: edges to a child with multiple parents. Such a child
        //   becomes executable only after the last parent decrements its counter to
        //   zero at runtime.
        //
        // Phase 1: Build the dependency graph and instantiate tasks.
        // - child_to_parents: TaskId(child) -> set of parent TaskId's this child
        //   subscribes to.
        // - parent_to_children: TaskId(parent) -> set of children TaskId's subscribed
        //   to this parent.
        // The maps are filled by Task::setup(...) via SetupApi::subscribe(...) calls.
        // We also fully construct all Task instances and keep them in `tasks` keyed by
        // TaskId.
        let max_cap = task_configs.len().saturating_sub(1);
        let mut child_to_parents = IndexMap::with_capacity_and_hasher(max_cap, FxBuildHasher);
        let mut parent_to_children = IndexMap::with_capacity_and_hasher(max_cap, FxBuildHasher);
        let mut tasks = IndexMap::with_capacity_and_hasher(task_configs.len(), FxBuildHasher);
        for (&current_task_id, config) in task_configs {
            let mut setup_api = TaskSetupApiImpl::<C> {
                current_task_id,
                task_configs,
                child_to_parents: &mut child_to_parents,
                parent_to_children: &mut parent_to_children,
            };
            let task = Task::setup(&mut setup_api, config);
            tasks.insert(current_task_id, task);
        }

        // Phase 2: Seed topological order with roots (independent tasks).
        // Roots are tasks that don't appear in child_to_parents (i.e., have no
        // parents). We assign them consecutive `task_idx` from 0 and set
        // `degree = 0`. `degree` represents the longest distance from any root
        // (number of edges), and is used to compute downstream tasks' degrees
        // later.
        let mut task_info_map: IndexMap<_, _> = tasks
            .keys()
            .filter(|key| !child_to_parents.contains_key(*key))
            .copied()
            .enumerate()
            .map(|(idx, id)| {
                (
                    id,
                    TaskSetupLayout {
                        task_idx: idx.try_into().expect("Executor::new: [1]"),
                        degree: 0,
                        parents: None,
                        children: parent_to_children.get(&id),
                    },
                )
            })
            .collect();

        // If there are tasks but no roots, the entire graph must be cyclic.
        if task_info_map.is_empty() && !tasks.is_empty() {
            // No independent tasks were found. All connected components are cyclic.
            return Err(ExecutorSetupError::Cycle);
        }

        // Number of independent tasks, guaranteed to be at the beginning of the final
        // order.
        let num_independent_tasks = task_info_map.len().try_into().expect("Executor::new: [2]");

        // Phase 3: Prepare a worklist of candidate tasks whose parents might soon be
        // fully known. We push all children of the already-placed roots.
        // `pending_task_set` prevents duplicates.
        let max_cap = task_configs.len() - task_info_map.len();
        let mut pending_task_queue = VecDeque::with_capacity(max_cap);
        let mut pending_task_set = HashSet::with_capacity_and_hasher(max_cap, FxBuildHasher);

        for task_info in task_info_map.values() {
            let Some(children) = task_info.children else {
                continue;
            };
            for &child_id in children {
                if pending_task_set.insert(child_id) {
                    pending_task_queue.push_back(child_id);
                }
            }
        }

        // Phase 4: Kahn-like processing with progress detection.
        // We repeatedly pop a candidate child. If ALL of its parents are already
        // present in `task_info_map`, we can assign its `task_idx` (next
        // sequential index) and compute its `degree = 1 + max(parent.degree)`.
        // Then we enqueue its children.
        //
        // If at least one parent is still missing, we skip inserting this child for
        // now. It will be re-enqueued later when a missing parent gets inserted
        // (parents enqueue children).
        //
        // `last_progress_iter` and `last_progress_queue_len` provide a robust cycle
        // detector: if we make no progress for as many iterations as the queue
        // size at the last progress moment, the remaining subgraph must be
        // cyclic.
        let mut last_progress_iter = 0usize;
        let mut last_progress_queue_len = pending_task_queue.len();
        let mut iter = 0usize;

        'process_pending: while let Some(task_id) = pending_task_queue.pop_front() {
            let removed = pending_task_set.remove(&task_id);
            assert!(removed, "Executor::new: [3]");
            if iter
                .checked_sub(last_progress_iter)
                .expect("Executor::new: [4]")
                >= last_progress_queue_len
            {
                // Last task readiness was too long ago compared to the number of tasks in the
                // queue as of that moment. This means that the graph contains a
                // cycle.
                //
                // HINT: The absence of cycles in the graph means that we will see at least one
                // insertion of a fully resolved task in `last_progress_queue_len` iterations.
                return Err(ExecutorSetupError::Cycle);
            }
            iter = iter.checked_add(1).expect("Executor::new: [5]");
            // Start with degree 1 (child is at least one edge away from some parent).
            let mut degree = 1;
            let parents = child_to_parents.get(&task_id).expect("Executor::new: [6]");
            for parent_id in parents {
                let Some(parent_info) = task_info_map.get(parent_id) else {
                    // A parent is not yet resolved; defer this child until more parents appear.
                    continue 'process_pending;
                };
                // degree(child) = max(degree(child), 1 + degree(parent))
                let degree_candidate = parent_info
                    .degree
                    .checked_add(1)
                    .expect("Executor::new: [7]");
                if degree_candidate > degree {
                    degree = degree_candidate;
                }
            }
            let children = parent_to_children.get(&task_id);
            // All parents are known -> assign the next topological index and record links.
            let layout = TaskSetupLayout {
                task_idx: task_info_map.len().try_into().expect("Executor::new: [8]"),
                degree,
                parents: Some(parents),
                children,
            };
            let inserted_new = task_info_map.insert(task_id, layout).is_none();
            assert!(inserted_new, "Executor::new: [9]");

            // Enqueue this task's children for potential processing.
            if let Some(children) = children {
                for &child_id in children {
                    if pending_task_set.insert(child_id) {
                        pending_task_queue.push_back(child_id);
                    }
                }
            }
            last_progress_iter = iter;
            last_progress_queue_len = pending_task_queue.len();
        }
        drop(pending_task_queue);
        drop(pending_task_set);

        // If we didn't place all tasks, there exists at least one cyclic component.
        if task_info_map.len() != tasks.len() {
            // Graph contains cyclic connected components.
            return Err(ExecutorSetupError::Cycle);
        }

        // Phase 5: Materialize task and output slots in topological order.
        // Also classify edges into `owned_children` (child has exactly one parent)
        // and `shared_children` (child has multiple parents).
        let (task_slots, output_slots, parents) = task_info_map
            .iter()
            .enumerate()
            .map(|(idx, (task_id, task_info))| {
                let &TaskSetupLayout {
                    task_idx,
                    degree: _,
                    parents,
                    children,
                } = task_info;
                assert_eq!(idx, task_idx as usize, "Executor::new: [10]");
                let parents_total = parents
                    .map_or(0, IndexSet::len)
                    .try_into()
                    .expect("Executor::new: [11]");
                let mut owned_children = vec![];
                let mut shared_children = vec![];
                if let Some(children) = children {
                    for child in children {
                        let parents = &child_to_parents[child];
                        let idx = task_info_map[child].task_idx;
                        if parents.len() == 1 {
                            // If we're the only parent, this child can be started immediately
                            // after we finish without cross-thread synchronization.
                            assert_eq!(
                                parents.first().expect("Executor::new: [12]"),
                                task_id,
                                "Executor::new: [13]"
                            );
                            owned_children.push(idx);
                        } else {
                            // Otherwise the child is shared between multiple parents, and we will
                            // coordinate its readiness using atomic counters and fences at runtime.
                            assert!(parents.len() > 1, "Executor::new: [14]");
                            shared_children.push(idx);
                        }
                    }
                }
                // Sort children's indexes to achieve better cache locality.
                owned_children.sort_unstable();
                shared_children.sort_unstable();
                let task_slot = TaskSlot::new(TaskLayout {
                    task: tasks.swap_remove(task_id).expect("Executor::new: [15]"),
                    owned_children,
                    shared_children,
                });
                let output_slot = OutputSlot::<C>::new(None);
                let parents_info_slot = ParentInfoSlot {
                    parents_total,
                    parents_left: AtomicU16::new(parents_total),
                };
                (task_slot, output_slot, parents_info_slot)
            })
            .unzip3();

        assert!(tasks.is_empty(), "Executor::new: [16]");
        drop(tasks);

        // Phase 6: Build the final executor object with consistent indices and
        // invariants.
        let result = Self {
            tasks: task_slots,
            outputs: output_slots,
            parents,
            num_independent_tasks,
            task_id_to_index: task_info_map
                .into_iter()
                .map(|(task_id, info)| (task_id, info.task_idx))
                .collect(),
        };
        assert_eq!(
            result.tasks.len(),
            result.outputs.len(),
            "Executor::new: [17]"
        );
        assert_eq!(
            result.tasks.len(),
            result.parents.len(),
            "Executor::new: [18]"
        );
        assert_eq!(
            result.tasks.len(),
            result.task_id_to_index.len(),
            "Executor::new: [19]"
        );
        if !result.tasks.is_empty() {
            assert_ne!(result.num_independent_tasks, 0, "Executor::new: [20]");
        }
        Ok(result)
    }

    /// Execute the tasks in a parallel, dependency-respecting manner.
    ///
    /// Independent roots are joined in parallel. When a task finishes, it:
    /// - Optionally performs a Release fence if it has shared children,
    /// - Decrements each shared child's `parents_left` (Relaxed),
    /// - If a decrement observes the counter reach zero, appends that child to
    ///   the owned set and performs an Acquire fence before executing it.
    ///
    /// This protocol ensures the child observes all parents' outputs while
    /// avoiding unnecessary barriers for purely owned edges.
    #[cfg(not(feature = "loom"))]
    pub fn execute(&mut self) {
        let Self {
            tasks,
            outputs,
            parents,
            num_independent_tasks,
            task_id_to_index,
        } = self;
        let exec_api = TaskExecApiImpl {
            outputs,
            task_id_to_index,
            _marker: PhantomData,
        };
        let independent_task_range = 0..*num_independent_tasks as usize;
        // SAFETY: Slices and the range are derived from `self` and satisfy the
        // preconditions of `join_independent_tasks`: indices are in-bounds and refer
        // only to independent tasks.
        unsafe {
            join_independent_tasks(tasks, outputs, parents, independent_task_range, exec_api);
        }
    }

    /// Loom-testable version of `execute`.
    #[cfg(feature = "loom")]
    pub fn execute(self) -> Self {
        use std::sync::Arc;

        let Self {
            tasks,
            outputs,
            parents,
            num_independent_tasks,
            task_id_to_index,
        } = self;

        let tasks = Arc::new(tasks);
        let outputs = Arc::new(outputs);
        let parents = Arc::new(parents);
        let task_id_to_index = Arc::new(task_id_to_index);

        let exec_api = TaskExecApiImpl {
            outputs: outputs.clone(),
            task_id_to_index: task_id_to_index.clone(),
            _marker: PhantomData,
        };
        let independent_task_range = 0..num_independent_tasks as usize;
        // SAFETY: The `Arc`-backed slices and index range satisfy the preconditions of
        // `join_independent_tasks`; roots are in-bounds and independent.
        unsafe {
            join_independent_tasks(
                tasks.clone(),
                exec_api.outputs.clone(),
                parents.clone(),
                independent_task_range,
                exec_api,
            );
        }

        let tasks = Arc::into_inner(tasks).unwrap();
        let outputs = Arc::into_inner(outputs).unwrap();
        let parents = Arc::into_inner(parents).unwrap();
        let task_id_to_index = Arc::into_inner(task_id_to_index).unwrap();

        Self {
            tasks,
            outputs,
            parents,
            num_independent_tasks,
            task_id_to_index,
        }
    }
}
