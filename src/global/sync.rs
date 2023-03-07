use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicU64, Ordering},
};

use allocator_api2::alloc::Allocator;

use crate::{sync::SyncBlinkAlloc, LocalBlinkAlloc};

switch_std_default! {
    /// [`GlobalAlloc`] implementation based on [`SyncBlinkAlloc`].
    ///
    /// # Example
    ///
    /// ```
    /// use blink_alloc::GlobalBlinkAlloc;
    ///
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: GlobalBlinkAlloc = GlobalBlinkAlloc::new();
    ///
    /// fn main() {
    ///     let _ = Box::new(42);
    ///     let _ = vec![1, 2, 3];
    /// }
    /// ```
    pub struct GlobalBlinkAlloc<A: Allocator = +std::alloc::System> {
        inner: UnsafeCell<SyncBlinkAlloc<A>>,
        #[cfg(debug_assertions)]
        allocations: AtomicU64,
    }
}

unsafe impl<A: Allocator + Send> Send for GlobalBlinkAlloc<A> {}
unsafe impl<A: Allocator + Sync> Sync for GlobalBlinkAlloc<A> {}

#[cfg(feature = "std")]
impl GlobalBlinkAlloc<std::alloc::System> {
    /// Create a new [`GlobalBlinkAlloc`].
    ///
    /// Const function can be used to initialize a static variable.
    ///
    /// # Example
    ///
    /// ```
    /// use blink_alloc::GlobalBlinkAlloc;
    ///
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: GlobalBlinkAlloc = GlobalBlinkAlloc::new();
    ///
    /// fn main() {
    ///     let _ = Box::new(42);
    ///     let _ = vec![1, 2, 3];
    /// }
    /// ```
    pub const fn new() -> Self {
        GlobalBlinkAlloc::new_in(std::alloc::System)
    }

    /// Create a new [`GlobalBlinkAlloc`].
    /// With this method you can specify initial chunk size.
    ///
    /// Const function can be used to initialize a static variable.
    ///
    /// # Example
    ///
    /// ```
    /// use blink_alloc::GlobalBlinkAlloc;
    ///
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: GlobalBlinkAlloc = GlobalBlinkAlloc::with_chunk_size(1024);
    ///
    /// fn main() {
    ///     let _ = Box::new(42);
    ///     let _ = vec![1, 2, 3];
    /// }
    /// ```
    pub const fn with_chunk_size(chunk_size: usize) -> Self {
        GlobalBlinkAlloc::with_chunk_size_in(chunk_size, std::alloc::System)
    }
}

impl<A> GlobalBlinkAlloc<A>
where
    A: Allocator,
{
    /// Create a new [`GlobalBlinkAlloc`]
    /// with specified underlying allocator.
    ///
    /// Const function can be used to initialize a static variable.
    ///
    /// # Example
    ///
    /// ```
    /// use blink_alloc::GlobalBlinkAlloc;
    ///
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: GlobalBlinkAlloc<std::alloc::System> = GlobalBlinkAlloc::new_in(std::alloc::System);
    ///
    /// fn main() {
    ///     let _ = Box::new(42);
    ///     let _ = vec![1, 2, 3];
    /// }
    /// ```
    pub const fn new_in(allocator: A) -> Self {
        GlobalBlinkAlloc {
            inner: UnsafeCell::new(SyncBlinkAlloc::new_in(allocator)),
            #[cfg(debug_assertions)]
            allocations: AtomicU64::new(0),
        }
    }

    /// Create a new [`GlobalBlinkAlloc`]
    /// with specified underlying allocator.
    /// With this method you can specify initial chunk size.
    ///
    /// Const function can be used to initialize a static variable.
    ///
    /// # Example
    ///
    /// ```
    /// use blink_alloc::GlobalBlinkAlloc;
    ///
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: GlobalBlinkAlloc<std::alloc::System> = GlobalBlinkAlloc::with_chunk_size_in(1024, std::alloc::System);
    ///
    /// fn main() {
    ///     let _ = Box::new(42);
    ///     let _ = vec![1, 2, 3];
    /// }
    /// ```
    pub const fn with_chunk_size_in(chunk_size: usize, allocator: A) -> Self {
        GlobalBlinkAlloc {
            inner: UnsafeCell::new(SyncBlinkAlloc::with_chunk_size_in(chunk_size, allocator)),
            #[cfg(debug_assertions)]
            allocations: AtomicU64::new(0),
        }
    }

    /// Resets this allocator, deallocating all chunks except the last one.
    /// Last chunk will be reused.
    /// With steady memory usage after few iterations
    /// one chunk should be sufficient for all allocations between resets.
    ///
    /// # Safety
    ///
    /// Memory allocated from this allocator becomes invalidated.
    /// The user is responsible to ensure that previously allocated memory
    /// won't be used after reset.
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "std")] fn main() {
    /// use blink_alloc::UnsafeGlobalBlinkAlloc;
    ///
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: UnsafeGlobalBlinkAlloc = unsafe { UnsafeGlobalBlinkAlloc::new() };
    ///
    /// unsafe { GLOBAL_ALLOC.scope() };
    ///
    /// let b = Box::new(42);
    /// let v = vec![1, 2, 3];
    /// drop(b);
    /// drop(v);
    ///
    /// // Safety: memory allocated after `scope` call won't be used after reset.
    /// unsafe { GLOBAL_ALLOC.reset() };
    /// # }
    /// # #[cfg(not(feature = "std"))] fn main() {}
    /// ```
    #[inline(always)]
    pub unsafe fn reset(&self) {
        #[cfg(debug_assertions)]
        {
            assert_eq!(
                self.allocations.load(Ordering::SeqCst),
                0,
                "Not everything was deallocated"
            );
        }

        (*self.inner.get()).reset_unchecked();
    }

    /// Refreshes scope of this allocator.
    ///
    /// Forgets previous allocations so that deallocating them will have no effect.
    /// Additionally resets will not invalidate allocations made before this call.
    ///
    /// When used as `#[global_allocator]` this method should be called
    /// at least once after `main` starts if `reset` is going to be ever called.
    ///
    /// # Safety
    ///
    /// This method is not thread-local.
    /// Caller must externally synchronize this method call with other threads
    /// accessing this allocator.
    #[inline(always)]
    pub unsafe fn scope(&self) {
        let allocator = core::ptr::read(self.inner.get()).into_inner();
        core::ptr::write(self.inner.get(), SyncBlinkAlloc::new_in(allocator));

        #[cfg(debug_assertions)]
        {
            self.allocations.store(0, Ordering::SeqCst);
        }
    }

    /// Refreshes scope of this allocator.
    /// With this method you can specify initial chunk size.
    ///
    /// Forgets previous allocations so that deallocating them will have no effect.
    /// Additionally resets will not invalidate allocations made before this call.
    ///
    /// When used as `#[global_allocator]` this method should be called
    /// at least once after `main` starts if `reset` is going to be ever called.
    ///
    /// # Safety
    ///
    /// This method is not thread-local.
    /// Caller must externally synchronize this method call with other threads
    /// accessing this allocator.
    #[inline(always)]
    pub unsafe fn scope_with_chunk_size(&self, chunk_size: usize) {
        let allocator = core::ptr::read(self.inner.get()).into_inner();
        core::ptr::write(
            self.inner.get(),
            SyncBlinkAlloc::with_chunk_size_in(chunk_size, allocator),
        );

        #[cfg(debug_assertions)]
        {
            self.allocations.store(0, Ordering::SeqCst);
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
    /// ```
    /// # #![cfg_attr(feature = "nightly", feature(allocator_api))]
    /// # use blink_alloc::GlobalBlinkAlloc;
    /// # use allocator_api2::vec::Vec;
    /// # fn main() {
    /// static BLINK: GlobalBlinkAlloc = GlobalBlinkAlloc::new();
    /// for _ in 0..3 {
    ///     for i in 0..16 {
    ///         let mut blink = BLINK.local(); // Sendable and 'static.
    ///         std::thread::scope(move |_| {
    ///             let mut vec = Vec::new_in(&blink);
    ///             vec.push(i);
    ///             for j in i*2..i*30 {
    ///                 vec.push(j); // Proxy will allocate enough memory to grow vec without reallocating on 2nd iteration and later.
    ///             }
    ///             drop(vec); // Without this line it will fail to borrow mutable on next line.
    ///             blink.reset();
    ///         });
    ///
    ///         // Safety: Proxies and allocations are dropped.
    ///         unsafe { BLINK.reset() };
    ///     }
    /// }
    /// # }
    /// ```
    pub fn local(&self) -> LocalBlinkAlloc<'_, A> {
        unsafe { &*self.inner.get() }.local()
    }
}

unsafe impl<A> GlobalAlloc for GlobalBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline]
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        match (*self.inner.get()).allocate(layout) {
            Ok(ptr) => {
                #[cfg(debug_assertions)]
                {
                    self.allocations.fetch_add(1, Ordering::SeqCst);
                }
                ptr.as_ptr().cast()
            }
            Err(_) => null_mut(),
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        let ptr = NonNull::new_unchecked(ptr);
        (*self.inner.get()).deallocate(ptr, layout.size());
        #[cfg(debug_assertions)]
        {
            let _ = self
                .allocations
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |a| {
                    Some(a.saturating_sub(1))
                });
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: core::alloc::Layout) -> *mut u8 {
        match (*self.inner.get()).allocate_zeroed(layout) {
            Ok(ptr) => {
                #[cfg(debug_assertions)]
                {
                    self.allocations.fetch_add(1, Ordering::SeqCst);
                }
                ptr.as_ptr().cast()
            }
            Err(_) => null_mut(),
        }
    }

    #[inline]
    unsafe fn realloc(
        &self,
        ptr: *mut u8,
        layout: core::alloc::Layout,
        new_size: usize,
    ) -> *mut u8 {
        let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
            return null_mut();
        };

        let result = match NonNull::new(ptr) {
            None => (*self.inner.get()).allocate(new_layout),
            Some(ptr) => (*self.inner.get()).resize(ptr, layout, new_layout),
        };

        match result {
            Ok(ptr) => ptr.as_ptr().cast(),
            Err(_) => null_mut(),
        }
    }
}
