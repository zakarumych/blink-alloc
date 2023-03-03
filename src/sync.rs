//! This module provides single-threaded blink allocator.

use core::{
    alloc::Layout,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use allocator_api2::alloc::{AllocError, Allocator};

#[cfg(feature = "alloc")]
use allocator_api2::alloc::Global;

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
    /// Deallocation is no-op. [`BlinkAllocator`] can be reset
    /// to free all chunks except the last one, that will be reused.
    ///
    /// Blink allocator aims to allocate a chunk large enough to
    /// serve all allocations between resets.
    ///
    /// A shared and mutable reference to the [`SyncBlinkAlloc`] implement
    /// [`Allocator`] trait.
    /// When "nightly" feature is enabled, [`Allocator`] trait is
    /// [`core::alloc::Allocator`]. Otherwise it is duplicated trait defined
    /// in [`allocator-api2`](allocator_api2).
    ///
    /// Resetting blink allocator requires mutable borrow, so it is not possible
    /// to do while shared borrow is alive. That matches requirement of
    /// [`Allocator`] trait - while [`Allocator`] instance
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
    /// # use std::ptr::NonNull;
    ///
    /// let mut blink = BlinkAlloc::new();
    /// let layout = std::alloc::Layout::new::<[u32; 8]>();
    /// let ptr = blink.allocate(layout).unwrap();
    /// let ptr = NonNull::new(ptr.as_ptr() as *mut u8).unwrap(); // Method for this is unstable.
    ///
    /// unsafe {
    ///     std::ptr::write(ptr.as_ptr().cast(), [1, 2, 3, 4, 5, 6, 7, 8]);
    ///     blink.deallocate(ptr, layout.size());
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
    /// # use allocator_api2::vec::Vec;
    /// # fn main() {
    /// let mut blink = BlinkAlloc::new();
    /// let mut vec = Vec::new_in(&blink);
    /// vec.push(1);
    /// vec.extend(1..3);
    /// vec.extend(3..10);
    /// drop(vec);
    /// blink.reset();
    /// # }
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
    #[inline(always)]
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
    #[inline(always)]
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
    #[inline(always)]
    pub const fn new_in(allocator: A) -> Self {
        SyncBlinkAlloc {
            arena: ArenaSync::new(),
            allocator,
            max_local_alloc: AtomicUsize::new(0),
        }
    }

    /// Creates new blink allocator that uses global allocator
    /// to allocate memory chunks.
    /// With this method you can specify initial chunk size.
    ///
    /// See [`SyncBlinkAlloc::new_in`] for using custom allocator.
    #[inline(always)]
    pub const fn with_chunk_size_in(chunk_size: usize, allocator: A) -> Self {
        SyncBlinkAlloc {
            arena: ArenaSync::with_chunk_size(chunk_size),
            allocator,
            max_local_alloc: AtomicUsize::new(0),
        }
    }

    /// Creates a new thread-local blink allocator proxy
    /// that borrows from this multi-threaded allocator.
    ///
    /// The local proxy allocator works faster and
    /// allows more consistent memory reuse.
    /// It can be recreated without resetting the multi-threaded allocator,
    /// allowing [`SyncBlinkAlloc`] to be warm-up and serve all allocations
    /// from a single chunk without ever blocking.
    ///
    /// Best works for fork-join style of parallelism.
    /// Create a local allocator for each thread/task.
    /// Reset after all threads/tasks are finished.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # #![cfg_attr(feature = "nightly", feature(allocator_api))]
    /// # use blink_alloc::{BlinkAlloc, SyncBlinkAlloc, BlinkAllocator};
    /// # use std::vec::Vec;
    /// # #[cfg(feature = "nightly")]
    /// # fn main() {
    /// let mut blink = SyncBlinkAlloc::new();
    /// for _ in 0..3 {
    ///     for i in 0..16 {
    ///         std::thread::scope(|_| {
    ///             let blink = blink.local();
    ///             let mut vec = Vec::new_in(&blink);
    ///             vec.push(i);
    ///             for j in i*2..i*30 {
    ///                 vec.push(j); // Proxy will allocate enough memory to grow vec without reallocating on 2nd iteration and later.
    ///             }
    ///         });
    ///     }
    ///     blink.reset();
    /// }
    /// # }
    /// # #[cfg(not(feature = "nightly"))]
    /// # fn main() {}
    /// ```
    #[inline(always)]
    pub fn local(&self) -> LocalBlinkAlloc<A> {
        LocalBlinkAlloc {
            arena: ArenaLocal::with_chunk_size(self.max_local_alloc.load(Ordering::Relaxed)),
            shared: self,
        }
    }

    /// Allocates memory with specified layout from this allocator.
    /// If needed it will allocate new chunk using underlying allocator.
    /// If chunk allocation fails, it will return `Err`.
    #[inline(always)]
    pub fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe { self.arena.alloc::<false>(layout, &self.allocator) }
    }

    /// Behaves like [`allocate`](SyncBlinkAlloc::allocate), but also ensures that the returned memory is zero-initialized.
    #[inline(always)]
    pub fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe { self.arena.alloc::<true>(layout, &self.allocator) }
    }

    /// Resizes memory allocation.
    /// Potentially happens in-place.
    ///
    /// # Safety
    ///
    /// `ptr` must be a pointer previously returned by [`allocate`](SyncBlinkAlloc::allocate).
    /// `old_size` must be in range `layout.size()..=slice.len()`
    /// where `layout` is the layout used in the call to [`allocate`](SyncBlinkAlloc::allocate).
    /// and `slice` is the slice pointer returned by [`allocate`](SyncBlinkAlloc::allocate).
    ///
    /// On success, the old pointer is invalidated and the new pointer is returned.
    /// On error old allocation is still valid.
    #[inline(always)]
    pub unsafe fn resize(
        &self,
        ptr: NonNull<u8>,
        old_size: usize,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe {
            self.arena
                .resize::<false>(ptr, old_size, new_layout, &self.allocator)
        }
    }

    /// Behaves like [`resize`](SyncBlinkAlloc::resize), but also ensures that the returned memory is zero-initialized.
    ///
    /// # Safety
    ///
    /// See [`resize`](SyncBlinkAlloc::resize) for safety requirements.
    #[inline(always)]
    pub unsafe fn resize_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_size: usize,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe {
            self.arena
                .resize::<true>(ptr, old_size, new_layout, &self.allocator)
        }
    }

    /// Deallocates memory previously allocated from this allocator.
    ///
    /// This call may not actually free memory.
    /// All memory is guaranteed to be freed on [`reset`](SyncBlinkAlloc::reset) call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a pointer previously returned by [`allocate`](SyncBlinkAlloc::allocate).
    /// `size` must be in range `layout.size()..=slice.len()`
    /// where `layout` is the layout used in the call to [`allocate`](SyncBlinkAlloc::allocate).
    /// and `slice` is the slice pointer returned by [`allocate`](SyncBlinkAlloc::allocate).
    #[inline(always)]
    pub unsafe fn deallocate(&self, ptr: NonNull<u8>, size: usize) {
        // Safety:
        // `ptr` was allocated by this allocator.
        unsafe {
            self.arena.dealloc(ptr, size);
        }
    }

    /// Resets this allocator, deallocating all chunks except the last one.
    /// Last chunk will be reused.
    /// With steady memory usage after few iterations
    /// chunk size should be sufficient for all allocations between resets.
    #[inline(always)]
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
    #[inline(always)]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::allocate(self, layout)
    }

    #[inline(always)]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::allocate_zeroed(self, layout)
    }

    #[inline(always)]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::resize(&self, ptr, old_layout.size(), new_layout)
    }

    #[inline(always)]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::resize(self, ptr, old_layout.size(), new_layout)
    }

    #[inline(always)]
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::resize_zeroed(&self, ptr, old_layout.size(), new_layout)
    }

    #[inline(always)]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        SyncBlinkAlloc::deallocate(self, ptr, layout.size());
    }
}

unsafe impl<A> Allocator for &mut SyncBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline(always)]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::allocate(self, layout)
    }

    #[inline(always)]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::allocate_zeroed(self, layout)
    }

    #[inline(always)]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::resize(&self, ptr, old_layout.size(), new_layout)
    }

    #[inline(always)]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::resize(self, ptr, old_layout.size(), new_layout)
    }

    #[inline(always)]
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        SyncBlinkAlloc::resize_zeroed(&self, ptr, old_layout.size(), new_layout)
    }

    #[inline(always)]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        SyncBlinkAlloc::deallocate(self, ptr, layout.size());
    }
}

unsafe impl<A> BlinkAllocator for SyncBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline(always)]
    fn reset(&mut self) {
        SyncBlinkAlloc::reset(self)
    }
}

with_global_default! {
    /// Thread-local proxy for [`SyncBlinkAlloc`].
    ///
    /// Using proxy can yield better performance when
    /// it is possible to create proxy once to use for many allocations.
    ///
    /// See [`SyncBlinkAlloc::local`] for more details.
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
    /// If needed it will allocate new chunk using underlying allocator.
    /// If chunk allocation fails, it will return `Err`.
    #[inline(always)]
    pub fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe { self.arena.alloc::<false>(layout, self.shared) }
    }

    /// Behaves like [`allocate`](LocalBlinkAlloc::allocate), but also ensures that the returned memory is zero-initialized.
    #[inline(always)]
    pub fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe { self.arena.alloc::<true>(layout, self.shared) }
    }

    /// Resizes memory allocation.
    /// Potentially happens in-place.
    ///
    /// # Safety
    ///
    /// `ptr` must be a pointer previously returned by [`allocate`](LocalBlinkAlloc::allocate).
    /// `old_size` must be in range `layout.size()..=slice.len()`
    /// where `layout` is the layout used in the call to [`allocate`](LocalBlinkAlloc::allocate).
    /// and `slice` is the slice pointer returned by [`allocate`](LocalBlinkAlloc::allocate).
    ///
    /// On success, the old pointer is invalidated and the new pointer is returned.
    /// On error old allocation is still valid.
    #[inline(always)]
    pub unsafe fn resize(
        &self,
        ptr: NonNull<u8>,
        old_size: usize,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe {
            self.arena
                .resize::<false>(ptr, old_size, new_layout, self.shared)
        }
    }

    /// Behaves like [`resize`](LocalBlinkAlloc::resize), but also ensures that the returned memory is zero-initialized.
    ///
    /// # Safety
    ///
    /// See [`resize`](LocalBlinkAlloc::resize) for safety requirements.
    #[inline(always)]
    pub unsafe fn resize_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_size: usize,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // Safety:
        // Same instance is used for all allocations and resets.
        unsafe {
            self.arena
                .resize::<true>(ptr, old_size, new_layout, self.shared)
        }
    }

    /// Deallocates memory previously allocated from this allocator.
    ///
    /// This call may not actually free memory.
    /// All memory is guaranteed to be freed on [`reset`](LocalBlinkAlloc::reset) call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a pointer previously returned by [`allocate`](LocalBlinkAlloc::allocate).
    /// `size` must be in range `layout.size()..=slice.len()`
    /// where `layout` is the layout used in the call to [`allocate`](LocalBlinkAlloc::allocate).
    /// and `slice` is the slice pointer returned by [`allocate`](LocalBlinkAlloc::allocate).
    #[inline(always)]
    pub unsafe fn deallocate(&self, ptr: NonNull<u8>, size: usize) {
        // Safety:
        // `ptr` was allocated by this allocator.
        unsafe {
            self.arena.dealloc(ptr, size);
        }
    }

    /// Resets this allocator, deallocating all chunks except the last one.
    /// Last chunk will be reused.
    /// With steady memory usage after few iterations
    /// chunk size should be sufficient for all allocations between resets.
    #[inline(always)]
    pub fn reset(&mut self) {
        self.arena.reset_leak(false);
    }
}

unsafe impl<A> Allocator for LocalBlinkAlloc<'_, A>
where
    A: Allocator,
{
    #[inline(always)]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        LocalBlinkAlloc::allocate(self, layout)
    }

    #[inline(always)]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        LocalBlinkAlloc::allocate_zeroed(self, layout)
    }

    #[inline(always)]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        LocalBlinkAlloc::resize(&self, ptr, old_layout.size(), new_layout)
    }

    #[inline(always)]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        LocalBlinkAlloc::resize(self, ptr, old_layout.size(), new_layout)
    }

    #[inline(always)]
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        LocalBlinkAlloc::resize_zeroed(&self, ptr, old_layout.size(), new_layout)
    }

    #[inline(always)]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        LocalBlinkAlloc::deallocate(&self, ptr, layout.size())
    }
}

unsafe impl<A> Allocator for &mut LocalBlinkAlloc<'_, A>
where
    A: Allocator,
{
    #[inline(always)]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        LocalBlinkAlloc::allocate(&**self, layout)
    }

    #[inline(always)]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        LocalBlinkAlloc::deallocate(&self, ptr, layout.size())
    }
}

unsafe impl<A> BlinkAllocator for LocalBlinkAlloc<'_, A>
where
    A: Allocator,
{
    #[inline(always)]
    fn reset(&mut self) {
        LocalBlinkAlloc::reset(self)
    }
}
