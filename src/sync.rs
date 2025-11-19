#[cfg(feature = "loom")]
mod imp {
    use crate::executor::{OutputSlot, ParentInfoSlot, TaskSlot};
    pub(crate) use loom::{
        cell::UnsafeCell,
        sync::atomic::{AtomicU16, Ordering, fence},
        thread,
    };
    pub(crate) use std::sync::Arc;

    pub(crate) type TaskSlots<'a, C> = Arc<Vec<TaskSlot<C>>>;
    pub(crate) type OutputSlots<'a, T> = Arc<Vec<OutputSlot<T>>>;
    pub(crate) type ParentInfoSlots<'a> = Arc<Vec<ParentInfoSlot>>;

    pub(crate) fn join(lhs: impl FnOnce() + Send + 'static, rhs: impl FnOnce() + Send + 'static) {
        let lhs = thread::spawn(lhs);
        let rhs = thread::spawn(rhs);
        lhs.join().unwrap();
        rhs.join().unwrap();
    }
}

#[cfg(not(feature = "loom"))]
mod imp {
    use crate::{
        executor::{OutputSlot, ParentInfoSlot, TaskSlot},
        types::SyncUnsafeCell,
    };
    pub(crate) use core::{
        cell::UnsafeCell,
        sync::atomic::{AtomicU16, Ordering, fence},
    };

    pub(crate) type TaskSlots<'a, C> = &'a [TaskSlot<C>];
    pub(crate) type OutputSlots<'a, T> = &'a [OutputSlot<T>];
    pub(crate) type ParentInfoSlots<'a> = &'a [ParentInfoSlot];

    pub(crate) fn join(lhs: impl FnOnce() + Send, rhs: impl FnOnce() + Send) {
        rayon::join(lhs, rhs);
    }

    pub(crate) trait LoomPtrCompat: Sized {
        type Ptr;
        fn with<R>(self, f: impl FnOnce(Self::Ptr) -> R) -> R;
    }

    impl<T> LoomPtrCompat for *const T {
        type Ptr = *const T;
        fn with<R>(self, f: impl FnOnce(Self::Ptr) -> R) -> R {
            f(self)
        }
    }

    impl<T> LoomPtrCompat for *mut T {
        type Ptr = *mut T;
        fn with<R>(self, f: impl FnOnce(Self::Ptr) -> R) -> R {
            f(self)
        }
    }

    impl<T> LoomPtrCompat for &mut T {
        type Ptr = *mut T;
        fn with<R>(self, f: impl FnOnce(Self::Ptr) -> R) -> R {
            f(self)
        }
    }

    pub(crate) trait LoomUnsafeCellCompat {
        type Target;
        /// # Safety
        ///
        /// Caller must ensure there is no concurrent access (mutable or
        /// immutable) that would violate Rust’s aliasing rules. The
        /// returned raw pointer must not outlive the underlying storage
        /// and must be used with the same exclusivity guarantees the
        /// executor provides.
        unsafe fn get_mut(&self) -> *mut Self::Target;
    }

    impl<T> LoomUnsafeCellCompat for SyncUnsafeCell<T> {
        type Target = T;

        unsafe fn get_mut(&self) -> *mut Self::Target {
            // SAFETY: `SyncUnsafeCell` is only accessed under executor-controlled
            // scheduling, which ensures exclusive logical access at this point.
            unsafe { self.get().as_mut().unwrap() }
        }
    }
}

pub(crate) use imp::*;
