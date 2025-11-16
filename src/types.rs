use crate::{config::Config, sync::UnsafeCell, task::Task};
use core::num::NonZeroU16;
use derive_more::{Deref, DerefMut};
use indexmap::{IndexMap as _IndexMap, IndexSet as _IndexSet};
use rustc_hash::FxBuildHasher;
use std::collections::{HashMap as _HashMap, HashSet as _HashSet};

/// A minimal `UnsafeCell` wrapper that is `Sync` when `T: Sync`.
///
/// Used internally by the executor to enable interior mutability across threads
/// while correctness is ensured by scheduling (no concurrent writers/readers on
/// the same slot in conflicting phases).
#[derive(Debug, Deref, DerefMut)]
#[repr(transparent)]
pub(crate) struct SyncUnsafeCell<T>(UnsafeCell<T>);

unsafe impl<T: Sync> Sync for SyncUnsafeCell<T> {}

impl<T> SyncUnsafeCell<T> {
    pub(crate) fn new(val: T) -> Self {
        Self(UnsafeCell::new(val))
    }
}

/// Unique identifier of a task in the DAG.
///
/// Compact `NonZeroU16` bounds the number of tasks and may enable minor
/// optimizations.
pub type TaskId = NonZeroU16;
/// User-provided configuration type for a task bound to a specific `Config`.
pub type TaskConfig<C> = <<C as Config>::Task as Task<C>>::Config;
/// Output type produced by a task after successful execution.
pub type TaskOutput<C> = <<C as Config>::Task as Task<C>>::Output;

pub(crate) type HashMap<K, V> = _HashMap<K, V, FxBuildHasher>;
pub(crate) type HashSet<T> = _HashSet<T, FxBuildHasher>;
/// `IndexMap` type with fast hasher.
pub type IndexMap<K, V> = _IndexMap<K, V, FxBuildHasher>;
pub(crate) type IndexSet<T> = _IndexSet<T, FxBuildHasher>;
