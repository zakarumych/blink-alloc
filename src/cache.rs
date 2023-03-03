use core::{
    cell::UnsafeCell,
    mem::{replace, ManuallyDrop, MaybeUninit},
    sync::atomic::{AtomicUsize, Ordering},
};

use alloc::vec::Vec;

use allocator_api2::alloc::{Allocator, Global};
use parking_lot::RwLock;

use crate::local::BlinkAlloc;

struct Inner<A: Allocator> {
    /// Array of [`BlinkAlloc`] instances ready to pop.
    pop_array: Vec<UnsafeCell<ManuallyDrop<BlinkAlloc<A>>>>,

    /// Index of the next pop [`BlinkAlloc`] instance to pop.
    next_pop: AtomicUsize,

    /// Array of [`BlinkAlloc`] instances that are released.
    push_array: Vec<UnsafeCell<MaybeUninit<BlinkAlloc<A>>>>,

    /// Index to push next [`BlinkAlloc`] instance.
    next_push: AtomicUsize,
}

unsafe impl<A> Sync for Inner<A>
where
    A: Allocator,
    BlinkAlloc<A>: Send,
{
}

/// Multi-thread cache for [`BlinkAlloc`] instances.
/// Stores pushed [`BlinkAlloc`] instances and returns them on pop.
/// Blink-allocators are kept warm in the cache.
///
/// This type is internally synchronized with hybrid
/// blocking + wait-free algorithm.
pub struct BlinkAllocCache<A: Allocator = Global> {
    inner: RwLock<Inner<A>>,
}

impl<A> Default for BlinkAllocCache<A>
where
    A: Allocator,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<A> BlinkAllocCache<A>
where
    A: Allocator,
{
    /// Creates a new empty [`BlinkAllocCache`].
    pub const fn new() -> Self {
        BlinkAllocCache {
            inner: RwLock::new(Inner {
                pop_array: Vec::new(),
                next_pop: AtomicUsize::new(0),
                push_array: Vec::new(),
                next_push: AtomicUsize::new(0),
            }),
        }
    }

    /// Acquires some [`BlinkAlloc`] instance from the cache.
    /// Returns none if the cache is empty.
    pub fn pop(&self) -> Option<BlinkAlloc<A>> {
        let inner = self.inner.read();

        if !inner.pop_array.is_empty() {
            // Acquire index to pop.
            let idx = inner.next_pop.fetch_add(1, Ordering::Acquire);

            if idx < inner.pop_array.len() {
                // Pop the [`BlinkAlloc`] instance.

                // Safety: Acquired exclusive index to this instance.
                let blink = unsafe { ManuallyDrop::take(&mut *inner.pop_array[idx].get()) };

                return Some(blink);
            }

            prevent_overflow(&inner.next_pop, idx, inner.pop_array.len());
        }

        if inner.next_push.load(Ordering::Relaxed) == 0 {
            return None;
        }

        drop(inner);
        let mut inner = self.inner.write();

        Self::flush(&mut inner);

        inner
            .pop_array
            .pop()
            .map(|cell| ManuallyDrop::into_inner(cell.into_inner()))
    }

    pub fn push(&self, blink: BlinkAlloc<A>) {
        let inner = self.inner.read();

        if !inner.push_array.is_empty() {
            // Acquire index to push.
            let idx = inner.next_push.fetch_add(1, Ordering::Acquire);

            if idx < inner.push_array.len() {
                // Push the [`BlinkAlloc`] instance.

                // Safety: Acquired exclusive index to this instance.
                MaybeUninit::write(unsafe { &mut *inner.push_array[idx].get() }, blink);
                return;
            }

            prevent_overflow(&inner.next_push, idx, inner.push_array.len());
        }

        drop(inner);
        let mut inner = self.inner.write();

        Self::flush(&mut inner);

        inner
            .pop_array
            .push(UnsafeCell::new(ManuallyDrop::new(blink)));
    }

    fn flush(inner: &mut Inner<A>) {
        let pushed = replace(inner.next_push.get_mut(), 0).min(inner.push_array.len());
        let popped = replace(inner.next_pop.get_mut(), 0).min(inner.pop_array.len());

        let pushed_iter = inner.push_array.drain(..pushed);

        inner.pop_array.splice(
            ..popped,
            pushed_iter.map(|cell| {
                // rustc, please, optimized this call to no-op.

                // Safety: `next_push` equals the number of elements for which push started.
                // Exclusive lock ensures initialization finished.
                UnsafeCell::new(ManuallyDrop::new(unsafe {
                    cell.into_inner().assume_init()
                }))
            }),
        );
    }
}

fn prevent_overflow(atomic: &AtomicUsize, current: usize, upper: usize) {
    #[cold]
    fn cold_store(atomic: &AtomicUsize, upper: usize) {
        atomic.store(upper, Ordering::Relaxed);
    }

    if current >= isize::MAX as usize {
        cold_store(atomic, upper);
    }
}
