//! This module provides single-threaded blink allocator.

use core::{
    alloc::Layout,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use allocator_api2::{AllocError, Allocator};

#[cfg(feature = "alloc")]
use allocator_api2::Global;

use crate::{
    api::BlinkAllocator,
    arena::{Arena, ArenaLocal, ArenaSync},
};

with_global_default! {
    /// Multi-threaded blink allocator.
    ///
    /// Blink-allocator is arena-based allocator that
    /// allocates memory in growing chunks and serve allocations from them.
    /// When chunk is exhausted a new larger chunk is allocated.
    ///
    /// Deallocation is no-op. `BlinkAllocator` can be reset
    /// to free all chunks except the last one, that will be reused.
    ///
    /// Blink allocator aims to allocate a chunk large enough to
    /// serve all allocations between resets.
    ///
    /// A shared and mutable reference to the `BlinkAlloc` implement
    /// `Allocator` trait.
    /// When "nightly" feature is enabled, `Allocator` trait is
    /// `core::alloc::Allocator`. Otherwise it is duplicated trait defined
    /// in this crate.
    ///
    /// Resetting blink allocator requires mutable borrow, so it is not possible
    /// to do while shared borrow is alive. That matches requirement of
    /// `Allocator` trait - while `Allocator` instance
    /// (a shared reference to `BlinkAlloc`) or any of its clones are alive,
    /// allocated memory must be valid.
    ///
    /// This version of blink-allocator is multi-threaded.
    /// It can be used from multiple threads concurrently to allocate memory.
    /// As mutable borrow is required to reset the allocator,
    /// it is not possible to do when shared.
    /// Internally it uses [`RwLock`] and [`AtomicUsize`] for synchronized
    /// interior mutability. [`RwLock`] is only write-locked when new chunk
    /// must be allocated. The arena allocation is performed using lock-free
    /// algorithm.
    ///
    /// Still it is slower than single-threaded version [`BlinkAlloc`].
    ///
    /// For best of both worlds [`LocalBlinkAlloc`] can be created from
    /// this allocator. [`LocalBlinkAlloc`] will allocate chunks from this
    /// allocator, but is single-threaded by itself.
    ///
    /// [`RwLock`]: parking_lot::RwLock
    /// [`AtomicUsize`]: core::sync::atomic::AtomicUsize
    /// [`BlinkAlloc`]: crate::local::BlinkAlloc
    /// [`LocalBlinkAlloc`]: crate::sync::LocalBlinkAlloc
    ///
    /// # Example
    ///
    /// ```
    /// # #![cfg_attr(feature = "nightly", feature(allocator_api))]
    /// # use blink_alloc::BlinkAlloc;
    /// # use allocator_api2::Allocator;
    /// # use std::ptr::NonNull;
    ///
    /// let mut blink = BlinkAlloc::new();
    /// let layout = std::alloc::Layout::new::<[u32; 8]>();
    /// let ptr = blink.allocate(layout).unwrap();
    /// let ptr = NonNull::new(ptr.as_ptr() as *mut u8).unwrap(); // Method for this is unstable.
    ///
    /// unsafe {
    ///     std::ptr::write(ptr.as_ptr().cast(), [1, 2, 3, 4, 5, 6, 7, 8]);
    ///     blink.deallocate(ptr, layout);
    /// }
    ///
    /// blink.reset();
    /// ```
    ///
    /// # Example that uses nightly's `allocator_api`
    ///
    /// ```
    /// # #![cfg_attr(feature = "nightly", feature(allocator_api))]
    /// # use blink_alloc::BlinkAlloc;
    /// # use std::vec::Vec;
    /// # #[cfg(feature = "nightly")]
    /// # fn main() {
    /// let mut blink = BlinkAlloc::new();
    /// let mut vec = Vec::new_in(&blink);
    /// vec.push(1);
    /// vec.extend(1..3);
    /// vec.extend(3..10);
    /// drop(vec);
    /// blink.reset();
    /// # }
    /// # #[cfg(not(feature = "nightly"))]
    /// # fn main() {}
    /// ```
    pub struct SyncBlinkAlloc<A: Allocator = +Global> {
        arena: ArenaSync,
        allocator: A,
        max_local_alloc: AtomicUsize,
    }
}

impl<A: Allocator> Drop for SyncBlinkAlloc<A> {
    fn drop(&mut self) {
        unsafe {
            self.arena.reset(false, &self.allocator);
        }
    }
}

#[test]
fn check_sync() {
    fn for_sync_alloc<A: Allocator + Sync>() {
        fn is_sink<T: Sync>() {}
        is_sink::<SyncBlinkAlloc<A>>();
    }
    for_sync_alloc::<Global>();
}

impl<A> Default for SyncBlinkAlloc<A>
where
    A: Allocator + Default,
{
    #[inline]
    fn default() -> Self {
        Self::new_in(Default::default())
    }
}

#[cfg(feature = "alloc")]
impl SyncBlinkAlloc<Global> {
    /// Creates new blink allocator that uses global allocator
    /// to allocate memory chunks.
    ///
    /// See [`SyncBlinkAlloc::new_in`] for using custom allocator.
    #[inline]
    pub const fn new() -> Self {
        SyncBlinkAlloc::new_in(Global)
    }
}

impl<A> SyncBlinkAlloc<A>
where
    A: Allocator,
{
    /// Creates new blink allocator that uses provided allocator
    /// to allocate memory chunks.
    ///
    /// See [`SyncBlinkAlloc::new`] for using global allocator.
    #[inline]
    pub const fn new_in(allocator: A) -> Self {
        SyncBlinkAlloc {
            arena: ArenaSync::new(0),
            allocator,
            max_local_alloc: AtomicUsize::new(0),
        }
    }

    /// Creates a new thread-local blink allocator
    /// that borrows from this multi-threaded allocator.
    ///
    /// The local allocator works faster by not requiring atomic operations.
    /// And it can be recreated without resetting the multi-threaded allocator.
    /// Allowing `SyncBlinkAlloc` to be warm-up and serve all allocations
    /// from a single chunk without ever blocking.
    ///
    /// Best works for fork-join style of parallelism.
    /// Create a local allocator for each thread/task and drop on join.
    /// Reset after all threads/tasks are joined.
    ///
    /// # Examples
    ///
    /// ```
    /// # #![cfg_attr(feature = "nightly", feature(allocator_api))]
    /// # use blink_alloc::{BlinkAlloc, SyncBlinkAlloc, BlinkAllocator};
    /// # use std::vec::Vec;
    /// # #[cfg(feature = "nightly")]
    /// # fn main() {
    /// let mut blink = SyncBlinkAlloc::new();
    /// for i in 0..16 {
    ///     std::thread::scope(|_| {
    ///         let blink = blink.local();
    ///         let mut vec = Vec::new_in(&blink);
    ///         vec.push(i);
    ///         vec.extend(i * 2..i * 3);
    ///     });
    /// }
    /// blink.reset();
    /// # }
    /// # #[cfg(not(feature = "nightly"))]
    /// # fn main() {}
    /// ```
    pub fn local(&self) -> LocalBlinkAlloc<A> {
        LocalBlinkAlloc {
            arena: ArenaLocal::new(self.max_local_alloc.load(Ordering::Relaxed)),
            shared: self,
        }
    }

    /// Allocates memory with specified layout from this allocator.
    /// If needed it will allocate new chunk using underlying allocator.
    /// If chunk allocation fails, it will return `Err`.
    #[inline]
    pub fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe { self.arena.alloc(layout, &self.allocator) }
    }

    /// Resets this allocator, deallocating all chunks except the last one.
    /// Last chunk will be reused.
    /// With steady memory usage after few iterations
    /// chunk size should be sufficient for all allocations between resets.
    #[inline]
    pub fn reset(&mut self) {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe {
            self.arena.reset(true, &self.allocator);
        }
    }
}

unsafe impl<A> Allocator for SyncBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::allocate(self, layout)
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let _ = (ptr, layout);
    }
}

unsafe impl<A> Allocator for &mut SyncBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::allocate(&**self, layout)
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let _ = (ptr, layout);
    }
}

unsafe impl<A> BlinkAllocator for SyncBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline]
    fn reset(&mut self) {
        SyncBlinkAlloc::reset(self)
    }
}

with_global_default! {
    pub struct LocalBlinkAlloc<'a, A: Allocator = +Global> {
        arena: ArenaLocal,
        shared: &'a SyncBlinkAlloc<A>,
    }
}

impl<A> Drop for LocalBlinkAlloc<'_, A>
where
    A: Allocator,
{
    fn drop(&mut self) {
        self.arena.reset_leak(false);
        self.shared
            .max_local_alloc
            .fetch_max(self.arena.last_chunk_size(), Ordering::Relaxed);
    }
}

impl<A> LocalBlinkAlloc<'_, A>
where
    A: Allocator,
{
    /// Allocates memory with specified layout from this allocator.
    /// If needed it will allocate new chunk
    /// using underlying `SyncBlinkAlloc` allocator.
    /// If chunk allocation fails, it will return `Err`.
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe { self.arena.alloc(layout, self.shared) }
    }

    /// Resets this allocator, deallocating all chunks except the last one.
    /// Last chunk will be reused.
    /// With steady memory usage after few iterations
    /// chunk size should be sufficient for all allocations between resets.
    #[inline]
    pub fn reset(&mut self) {
        self.arena.reset_leak(true);
    }
}

unsafe impl<A> Allocator for LocalBlinkAlloc<'_, A>
where
    A: Allocator,
{
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        LocalBlinkAlloc::allocate(self, layout)
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let _ = (ptr, layout);
    }
}

unsafe impl<A> Allocator for &mut LocalBlinkAlloc<'_, A>
where
    A: Allocator,
{
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        LocalBlinkAlloc::allocate(&**self, layout)
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let _ = (ptr, layout);
    }
}

unsafe impl<A> BlinkAllocator for LocalBlinkAlloc<'_, A>
where
    A: Allocator,
{
    #[inline]
    fn reset(&mut self) {
        LocalBlinkAlloc::reset(self)
    }
}
