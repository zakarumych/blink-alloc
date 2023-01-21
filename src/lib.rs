//!
//! Blink-alloc is extremely fast allocator based on the idea of allocating
//! linearly and reset everything at once.
//!
//! # Design
//!
//! It is designed to pre-allocate a chunk of memory from lower-level allocator
//! and then allocate from it aiming to serve all allocation request
//! without re-allocating the chunk.
//! If pre-allocated memory is exhausted, it will allocate more bigger chunk
//! and free previous one on reset.
//! In few iterations it should enter a plateau where all memory can be allocated
//! from single chunk between resets. Unless behavior of the program changes.
//! If portion of pre-allocated memory is unused continuously on many iterations
//! allocator may free memory chunk and allocate smaller one.
//!
//! # Single-threaded
//!
//! `BlinkAlloc` is a single-threaded version of the allocator.
//! It requires no synchronization and works faster than others when on plateau.
//! `BlinkAlloc` can be sent to other threads inside `SendBlinkAlloc` wrapper
//! that ensures that `BlinkAlloc` is reset.
//!
//! Having `BlinkAlloc` per thread/task may be not practical or possible
//! due to API or program design. Consider alternatives.
//!
//! # Multi-threaded
//!
//! `SyncBlinkAlloc` works in multithreaded environment and shares memory
//! from one chunk between threads. Reduces overall memory usage,
//! skips warm-up phase for new threads and tasks.
//! However it still requires exclusive access for resettings.
//! Perfect for fork-join style of parallelism, where reset happens
//! after joining all threads.
//!
//! # Async
//!
//! For async environment this crate provides `BlinkCache` type
//! which is a cache of warmed `BlinkAlloc` instances.
//! Async tasks may fetch blink allocator from cache,
//! use it and then return it back to cache.
//! `BlinkCache` syncs usage stats between `BlinkAlloc` instances
//! to reduce inappropriate chunk size reduction.
//!
//! # Drop support
//!
//! All provided allocators support memory initialization API
//! for different scenarios, including simple value moving,
//! initialization from closure, manual initialization,
//! collection from iterators and more.
//! Methods come in two flavors: dropping and non-dropping. Default is dropping.
//! For non-dropping flavor use methods with `_and_forget` suffix.
//! For dropping flavor values will be properly dropped when allocator is reset.
//! With no overhead if `core::mem::needs_drop` returns `false`.
//! Otherwise dropping closure is be placed next to a value in memory
//! and linked to allocator's drop-list.
//! With non-dropping flavor values will not be accessed when memory is reset.
//! Which means that values can be uninitialized by user code.
//!
//! # Allocator API
//!
//! Allocators implement `Allocator` interface from `alloc` crate
//! or a copy of it when feature `nightly` is not enabled.
//! `nightly` requires Rust feature `allocator_api`
//! and works only on nightly.
//! Once `Allocator` trait is stable the feature will do nothing and
//! removed in next major release.
//!
//! # No-std
//!
//! This crate supports `no_std` environment.
//!

#![no_std]
// #![cfg_attr(feature = "nightly", feature(allocator_api, error_in_core))]

#[cfg(feature = "alloc")]
extern crate alloc;

mod align_up;
pub mod api;
mod blink;
mod drop_list;
mod st;

#[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
mod oom;

pub use self::{blink::Blink, st::BlinkAlloc};

#[cold]
fn cold() {}

struct BlinkAllocError<T> {
    value: T,
    layout: Option<core::alloc::Layout>,
}

pub(crate) trait ResultExt<T, E> {
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    fn handle_alloc_error(self) -> T;

    fn inner_error(self) -> Result<T, E>;
}

impl<T, E> ResultExt<T, E> for Result<T, BlinkAllocError<E>> {
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline(always)]
    fn handle_alloc_error(self) -> T {
        match self {
            Ok(value) => value,
            Err(error) => match error.layout {
                None => oom::size_overflow(),
                Some(layout) => oom::handle_alloc_error(layout),
            },
        }
    }

    #[inline(always)]
    fn inner_error(self) -> Result<T, E> {
        match self {
            Ok(value) => Ok(value),
            Err(error) => Err(error.value),
        }
    }
}
