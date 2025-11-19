# Concurrent Computation Graph (Rust)

___It's just a demo project.___

A high-performance executor for Directed Acyclic Graphs (DAG) of tasks with minimal synchronization overhead and a clear correctness story grounded in the C11 memory model. The executor:

- Builds a topological order and detects cycles at setup-time.
- Distinguishes children with a single parent (owned) from children with multiple parents (shared) to avoid unnecessary barriers.
- Uses a lightweight publish/subscribe protocol with carefully placed fences (Release/Relaxed/Acquire) to guarantee visibility of parents’ outputs.
- Includes Loom-based tests to exhaustively explore interleavings and catch data races and ordering bugs.


## Why

Traditional parallel pipelines often over-synchronize (coarse barriers, channels) or rely on heavyweight primitives that limit throughput. This executor:

- Computes a stable plan once (toposort + indexing) and then runs with near-zero overhead for owned edges.
- Uses the minimal fencing required for shared edges to ensure correctness without penalizing purely owned paths.


## How it works (high level)

1. Setup (topological plan):
   - Each task declares subscriptions to its parents during `Task::setup` using `SetupApi::subscribe`.
   - The executor builds two maps: child→parents and parent→children.
   - It seeds roots (tasks with no parents) and performs a Kahn-like traversal to assign a stable topological index to every task.
   - It classifies edges:
     - Owned child: child has exactly one parent (the current task). It can be launched immediately in the same thread after the parent completes.
     - Shared child: child has multiple parents. A lightweight atomic counter tracks remaining parents.

2. Execute (parallel, minimal barriers):
   - Independent roots are joined in parallel.
   - When a task completes:
     - If it has shared children, it performs a Release fence and then decrements each child’s `parents_left` with Relaxed ordering.
     - The last parent to decrement a child observes `fetch_sub(...) == 1`, adds the child to the owned set, and performs an Acquire fence before running it.
   - Owned children are executed in the same thread without additional barriers.

This protocol guarantees every child observes all of its parents’ results while avoiding unnecessary barriers along purely owned edges.


## Core API surface

- `task::Task`: implement for your domain-specific node.
  - `fn setup(api: &mut impl SetupApi, cfg: &Self::Config) -> Self`
  - `fn exec(&mut self, data: &impl ExecApi<C>) -> Self::Output`
- `executor::Executor`:
  - `fn setup(task_configs: &IndexMap<TaskId, TaskConfig<C>>) -> Result<Self, ExecutorSetupError>`
  - `fn execute(&mut self)` (non-Loom) / `fn execute(self) -> Self` (Loom)

Types:

- `types::TaskId` is `NonZeroU16` → at most 65,535 tasks per plan.
- `TaskId -> index` mapping is computed and stored for fast, stable access.


## Example

```rust
use concurrent_computation_graph::{
    config::Config,
    executor::Executor,
    task::{ExecApi, SetupApi, Task, TaskSubscriptionId},
    types::{IndexMap, TaskId},
};
use core::num::NonZeroU16;

fn tid(x: u16) -> TaskId { NonZeroU16::new(x).unwrap() }

#[derive(Clone)]
struct MyConfig;
impl Config for MyConfig { type Task = MyTask; }

#[derive(Clone)]
struct NodeCfg {
    id: TaskId,
    parents: Vec<TaskId>,
    value: u32,
}

#[derive(Debug, Clone)]
struct MyTask {
    subs: Vec<TaskSubscriptionId>,
    value: u32,
}

impl Task<MyConfig> for MyTask {
    type Config = NodeCfg;
    type Output = u32;

    fn setup(api: &mut impl SetupApi, cfg: &Self::Config) -> Self {
        let subs = cfg.parents.iter().copied().map(|p| api.subscribe(p)).collect();
        Self { subs, value: cfg.value }
    }

    fn exec(&mut self, data: &impl ExecApi<MyConfig>) -> Self::Output {
        let mut acc = self.value;
        for &sub in &self.subs {
            acc += *data.read(sub);
        }
        acc
    }
}

fn main() {
    let a = NodeCfg { id: tid(1), parents: vec![], value: 1 };
    let b = NodeCfg { id: tid(2), parents: vec![], value: 10 };
    let c = NodeCfg { id: tid(3), parents: vec![tid(1), tid(2)], value: 100 };

    let mut cfgs: IndexMap<TaskId, NodeCfg> = IndexMap::default();
    cfgs.insert(a.id, a);
    cfgs.insert(b.id, b);
    cfgs.insert(c.id, c);

    let mut exec = Executor::<MyConfig>::setup(&cfgs).expect("DAG must be acyclic");
    exec.execute();
}
```


## Building and testing

- Build:

```bash
cargo build
```

- Run default tests (non-Loom):

```bash
cargo test
```


## Running Loom tests

This repository includes `tests/loom.rs`, which is compiled and run only when the `loom` cfg is enabled. Loom replaces atomics and concurrency primitives with deterministic models to explore interleavings.

- The simplest way to run Loom tests:

```bash
RUSTFLAGS="--cfg loom" cargo test --test loom -- --nocapture
```

Tips:

- Control exploration via Loom environment variables, e.g.:

```bash
LOOM_MAX_PREEMPTIONS=2 RUSTFLAGS="--cfg loom" cargo test --test loom -- --nocapture
```


## Safety and memory model

The executor relies on a small set of unsafe operations wrapped with explicit preconditions (documented at call sites and via rustdoc “Safety” sections):

- Owned edges execute in the same thread that produced the parent’s output (no barriers required).
- Shared edges use:
  - Release fence before decrementing a child’s `parents_left` counter (Relaxed).
  - On the last decrement (observed as `fetch_sub(...) == 1`), the executing thread performs an Acquire fence before running the child.
- This establishes the necessary happens-before so that children read fully-initialized outputs from all parents.

The code is written to avoid data races by construction:

- Each task’s output slot is written exactly once, after which it is only read.
- Task scheduling ensures no concurrent mutable access to the same task/output across threads.


## Design limits

- `TaskId` is `NonZeroU16`, bounding the number of tasks per plan to ≤ 65,535.
- Topological “degree” counters also use `u16`.
- Extremely deep chains (very tall DAGs) may result in deep recursion along owned edges.


## Contributing

Issues and PRs are welcome. When proposing changes to synchronization logic, please include Loom tests that capture the intended invariants and guard against regressions.


## License

This project is distributed under terms specified by the repository’s license (add one if missing).

