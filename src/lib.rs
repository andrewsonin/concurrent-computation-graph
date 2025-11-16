//! Concurrent computation graph executor with minimal synchronization overhead.
//!
//! This crate provides a high-performance executor for Directed Acyclic Graphs
//! (DAGs) of tasks. It:
//! - Builds a topological order of tasks and detects cycles at setup time.
//! - Distinguishes between children with a single parent (owned) and those with
//!   multiple parents (shared) to minimize cross-thread synchronization.
//! - Uses a lightweight publish/subscribe protocol with carefully placed memory
//!   fences (Release/Relaxed/Acquire) so children observe all parents' outputs.
//!
//! Key modules:
//! - `config`: binds a concrete task type to the executor via the `Config`
//!   trait.
//! - `task`: defines the `Task` interface and the `SetupApi`/`ExecApi` used by
//!   tasks.
//! - `executor`: contains graph setup (toposort, cycle checks, indexing) and
//!   parallel execution.
//! - `types`: common aliases and the `SyncUnsafeCell` primitive used
//!   internally.
//!
//! Quick start:
//! 1. Implement `Config` with your `Task` type.
//! 2. Implement `Task::setup` to declare dependencies via
//!    `SetupApi::subscribe`, and `Task::exec` to compute an output, reading
//!    parents via `ExecApi::read`.
//! 3. Build a map from `TaskId` to task configs, call `Executor::setup`, then
//!    `execute`.
//!
//! The executor ensures that, when a child task runs, all its parents have
//! completed and published their outputs with the appropriate happens-before
//! relations. This allows fast, deterministic processing while avoiding
//! unnecessary barriers.

/// Public interface to configure the computation graph.
///
/// Exposes the `Config` trait which binds the task type for a concrete
/// instantiation of the executor.
pub mod config;
/// The concurrent DAG executor.
///
/// Contains graph setup (topological ordering, cycle detection, mapping
/// between `TaskId` and execution indices) and parallel execution logic
/// with memory fences to uphold correct happens-before relations.
pub mod executor;
mod sync;
/// Task definitions and the setup/exec APIs exposed to tasks.
///
/// Defines the `Task` trait (`new`, `exec`), the `SetupApi` used during
/// construction to subscribe to parents, and `ExecApi` used at runtime
/// to read outputs of completed parent tasks.
pub mod task;
/// Core types and utilities used across the crate (IDs, outputs, cell wrapper).
///
/// Provides `TaskId`, configuration/output aliases, and a `SyncUnsafeCell`
/// wrapper enabling interior mutability under the executor’s scheduling
/// guarantees.
pub mod types;
mod utils;
