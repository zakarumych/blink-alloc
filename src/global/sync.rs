use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::{null_mut, NonNull},
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
        inner: SyncBlinkAlloc<A>,
    }
}

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
        Self {
            inner: SyncBlinkAlloc::new_in(std::alloc::System),
        }
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
        Self {
            inner: SyncBlinkAlloc::with_chunk_size_in(chunk_size, std::alloc::System),
        }
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
        Self {
            inner: SyncBlinkAlloc::new_in(allocator),
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
        Self {
            inner: SyncBlinkAlloc::with_chunk_size_in(chunk_size, allocator),
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
    /// use blink_alloc::GlobalBlinkAlloc;
    ///
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: GlobalBlinkAlloc = GlobalBlinkAlloc::new();
    ///
    /// fn main() {
    ///     let b = Box::new(42);
    ///     let v = vec![1, 2, 3];
    ///     drop(b);
    ///     drop(v);
    ///
    ///     // Safety: allocated memory won't be used after reset.
    ///     unsafe { GLOBAL_ALLOC.reset() };
    /// }
    /// ```
    #[inline(always)]
    pub unsafe fn reset(&self) {
        self.inner.reset_unchecked();
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
        self.inner.local()
    }
}

unsafe impl<A> GlobalAlloc for GlobalBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline]
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        match self.inner.allocate(layout) {
            Ok(ptr) => ptr.as_ptr().cast(),
            Err(_) => null_mut(),
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        if let Some(ptr) = NonNull::new(ptr) {
            self.inner.deallocate(ptr, layout.size());
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: core::alloc::Layout) -> *mut u8 {
        match self.inner.allocate_zeroed(layout) {
            Ok(ptr) => ptr.as_ptr().cast(),
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
            None => self.inner.allocate(new_layout),
            Some(ptr) => self.inner.resize(ptr, layout, new_layout),
        };

        match result {
            Ok(ptr) => ptr.as_ptr().cast(),
            Err(_) => null_mut(),
        }
    }
}
