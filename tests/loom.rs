#![allow(missing_docs)]
#![cfg(feature = "loom")]

use ccg::{
    config::Config,
    executor::Executor,
    task::{ExecApi, SetupApi, Task, TaskSubscriptionId},
    types::{IndexMap, TaskId},
};
use core::num::NonZeroU16;
use loom::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn tid(x: u16) -> TaskId {
    NonZeroU16::new(x).unwrap()
}

#[derive(Debug, Clone)]
struct Shared {
    // Indexed by (TaskId::get() - 1)
    outputs: Arc<Mutex<Vec<Option<u32>>>>,
    counts: Arc<Vec<AtomicUsize>>,
}

impl Shared {
    fn new(capacity: usize) -> Self {
        let outputs = vec![None; capacity];
        let counts = (0..capacity).map(|_| AtomicUsize::new(0)).collect();
        Self {
            outputs: Arc::new(Mutex::new(outputs)),
            counts: Arc::new(counts),
        }
    }
    fn idx(id: TaskId) -> usize {
        (id.get() - 1) as usize
    }
}

#[derive(Clone)]
struct TestConfig;

impl Config for TestConfig {
    type Task = TestTask;
}

#[derive(Clone)]
struct NodeCfg {
    id: TaskId,
    parents: Vec<TaskId>,
    value: u32,
    shared: Shared,
}

#[derive(Debug, Clone)]
struct TestTask {
    id: TaskId,
    subs: Vec<TaskSubscriptionId>,
    value: u32,
    shared: Shared,
}

impl Task<TestConfig> for TestTask {
    type Config = NodeCfg;
    type Output = u32;

    fn setup(api: &mut impl SetupApi, cfg: &Self::Config) -> Self {
        let subs = cfg.parents.iter().map(|&p| api.subscribe(p)).collect();
        Self {
            id: cfg.id,
            subs,
            value: cfg.value,
            shared: cfg.shared.clone(),
        }
    }

    fn exec(&mut self, data: &impl ExecApi<TestConfig>) -> Self::Output {
        // Count executions (must be exactly once per task).
        let idx = Shared::idx(self.id);
        self.shared.counts[idx].fetch_add(1, Ordering::Relaxed);

        // Sum this node's value and the parents' values.
        let mut acc = self.value;
        for &sub in &self.subs {
            acc += *data.read(sub);
        }

        // Store the computed result for external assertions.
        {
            let mut outs = self.shared.outputs.lock().unwrap();
            outs[idx] = Some(acc);
        }
        acc
    }
}

#[test]
fn loom_diamond_correctness_and_single_exec() {
    loom::model(|| {
        // Graph:
        //   A(1)   B(2)
        //     \    /
        //       C(3)
        //        |
        //       D(4)
        // Values: A=1, B=10, C=100, D=1000
        // Expectation: C = 1 + 10 + 100 = 111; D = 111 + 1000 = 1111
        let shared = Shared::new(4);
        let a = NodeCfg {
            id: tid(1),
            parents: vec![],
            value: 1,
            shared: shared.clone(),
        };
        let b = NodeCfg {
            id: tid(2),
            parents: vec![],
            value: 10,
            shared: shared.clone(),
        };
        let c = NodeCfg {
            id: tid(3),
            parents: vec![tid(1), tid(2)],
            value: 100,
            shared: shared.clone(),
        };
        let d = NodeCfg {
            id: tid(4),
            parents: vec![tid(3)],
            value: 1000,
            shared: shared.clone(),
        };

        // Intentionally shuffle the key order to exercise topological sort.
        let mut cfgs: IndexMap<TaskId, NodeCfg> = IndexMap::default();
        cfgs.insert(c.id, c);
        cfgs.insert(a.id, a);
        cfgs.insert(d.id, d);
        cfgs.insert(b.id, b);

        let executor = Executor::<TestConfig>::setup(&cfgs).expect("setup must succeed");
        let _ = executor.execute();

        let outs = shared.outputs.lock().unwrap();
        assert_eq!(outs[Shared::idx(tid(1))], Some(1));
        assert_eq!(outs[Shared::idx(tid(2))], Some(10));
        assert_eq!(outs[Shared::idx(tid(3))], Some(111));
        assert_eq!(outs[Shared::idx(tid(4))], Some(1111));

        // Verify that each task executed exactly once.
        assert_eq!(
            shared.counts[Shared::idx(tid(1))].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            shared.counts[Shared::idx(tid(2))].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            shared.counts[Shared::idx(tid(3))].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            shared.counts[Shared::idx(tid(4))].load(Ordering::Relaxed),
            1
        );
    });
}

#[test]
fn loom_three_parents_visibility_and_single_exec() {
    loom::model(|| {
        // Graph:
        //   A(1)   B(2)   C(3)
        //       \   |   /
        //          D(4)
        // Values: A=1, B=2, C=4, D=8
        // Expectation: D = 1 + 2 + 4 + 8 = 15
        let shared = Shared::new(4);
        let a = NodeCfg {
            id: tid(1),
            parents: vec![],
            value: 1,
            shared: shared.clone(),
        };
        let b = NodeCfg {
            id: tid(2),
            parents: vec![],
            value: 2,
            shared: shared.clone(),
        };
        let c = NodeCfg {
            id: tid(3),
            parents: vec![],
            value: 4,
            shared: shared.clone(),
        };
        let d = NodeCfg {
            id: tid(4),
            parents: vec![tid(1), tid(2), tid(3)],
            value: 8,
            shared: shared.clone(),
        };

        // Shuffle order.
        let mut cfgs: IndexMap<TaskId, NodeCfg> = IndexMap::default();
        cfgs.insert(d.id, d);
        cfgs.insert(b.id, b);
        cfgs.insert(a.id, a);
        cfgs.insert(c.id, c);

        let executor = Executor::<TestConfig>::setup(&cfgs).expect("setup must succeed");
        let _ = executor.execute();

        let outs = shared.outputs.lock().unwrap();
        assert_eq!(outs[Shared::idx(tid(1))], Some(1));
        assert_eq!(outs[Shared::idx(tid(2))], Some(2));
        assert_eq!(outs[Shared::idx(tid(3))], Some(4));
        assert_eq!(outs[Shared::idx(tid(4))], Some(15));

        // Each task must execute exactly once.
        assert_eq!(
            shared.counts[Shared::idx(tid(1))].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            shared.counts[Shared::idx(tid(2))].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            shared.counts[Shared::idx(tid(3))].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            shared.counts[Shared::idx(tid(4))].load(Ordering::Relaxed),
            1
        );
    });
}

#[test]
fn loom_two_parents_two_shared_children() {
    loom::model(|| {
        // Graph:
        //   A(1)   B(2)
        //    | \ / |
        //    |  X  |
        //    | / \ |
        //   C(3)  D(4)
        // Values: A=1, B=10, C=100, D=1000
        // Expectations:
        //   C = 1 + 10 + 100  = 111
        //   D = 1 + 10 + 1000 = 1011
        //
        // This exercises two independent shared-children counters. Loom may schedule
        // different "last parents" per child, ensuring the acquire fence path is
        // robust in both directions.
        let shared = Shared::new(4);
        let a = NodeCfg {
            id: tid(1),
            parents: vec![],
            value: 1,
            shared: shared.clone(),
        };
        let b = NodeCfg {
            id: tid(2),
            parents: vec![],
            value: 10,
            shared: shared.clone(),
        };
        let c = NodeCfg {
            id: tid(3),
            parents: vec![tid(1), tid(2)],
            value: 100,
            shared: shared.clone(),
        };
        let d = NodeCfg {
            id: tid(4),
            parents: vec![tid(1), tid(2)],
            value: 1000,
            shared: shared.clone(),
        };

        // Shuffle order to exercise topological setup.
        let mut cfgs: IndexMap<TaskId, NodeCfg> = IndexMap::default();
        cfgs.insert(d.id, d);
        cfgs.insert(b.id, b);
        cfgs.insert(a.id, a);
        cfgs.insert(c.id, c);

        let executor = Executor::<TestConfig>::setup(&cfgs).expect("setup must succeed");
        let _ = executor.execute();

        let outs = shared.outputs.lock().unwrap();
        assert_eq!(outs[Shared::idx(tid(1))], Some(1));
        assert_eq!(outs[Shared::idx(tid(2))], Some(10));
        assert_eq!(outs[Shared::idx(tid(3))], Some(111));
        assert_eq!(outs[Shared::idx(tid(4))], Some(1011));

        // Each task must execute exactly once.
        assert_eq!(
            shared.counts[Shared::idx(tid(1))].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            shared.counts[Shared::idx(tid(2))].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            shared.counts[Shared::idx(tid(3))].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            shared.counts[Shared::idx(tid(4))].load(Ordering::Relaxed),
            1
        );
    });
}
