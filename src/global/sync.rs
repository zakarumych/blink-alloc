use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicU64, Ordering},
};

use allocator_api2::alloc::{AllocError, Allocator};

use crate::{cold, sync::SyncBlinkAlloc, LocalBlinkAlloc};

struct State<A: Allocator> {
    blink: SyncBlinkAlloc<A>,
    enabled: bool,
}

impl<A: Allocator> State<A> {
    #[inline(always)]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self.enabled {
            true => self.blink.allocate(layout),
            false => {
                cold();
                self.blink.inner().allocate(layout)
            }
        }
    }

    #[inline(always)]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self.enabled {
            true => self.blink.allocate_zeroed(layout),
            false => {
                cold();
                self.blink.inner().allocate_zeroed(layout)
            }
        }
    }

    #[inline(always)]
    unsafe fn resize(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        match self.enabled {
            true => self.blink.resize(ptr, old_layout, new_layout),
            false => {
                cold();
                if old_layout.size() >= new_layout.size() {
                    self.blink.inner().grow(ptr, old_layout, new_layout)
                } else {
                    self.blink.inner().shrink(ptr, old_layout, new_layout)
                }
            }
        }
    }

    #[inline(always)]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        match self.enabled {
            true => self.blink.deallocate(ptr, layout.size()),
            false => {
                cold();
                self.blink.inner().deallocate(ptr, layout)
            }
        }
    }
}

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
        state: UnsafeCell<State<A>>,
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
    ///
    /// This method allows to specify initial chunk size.
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
            state: UnsafeCell::new(State {
                blink: SyncBlinkAlloc::new_in(allocator),
                enabled: false,
            }),
            #[cfg(debug_assertions)]
            allocations: AtomicU64::new(0),
        }
    }

    /// Create a new [`GlobalBlinkAlloc`]
    /// with specified underlying allocator.
    ///
    /// This method allows to specify initial chunk size.
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
    pub const fn with_chunk_size_in(chunk_size: usize, allocator: A) -> Self {
        GlobalBlinkAlloc {
            state: UnsafeCell::new(State {
                blink: SyncBlinkAlloc::with_chunk_size_in(chunk_size, allocator),
                enabled: false,
            }),
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
    /// Memory allocated from this allocator in blink mode becomes invalidated.
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
    /// unsafe { GLOBAL_ALLOC.blink_mode() };
    ///
    /// let b = Box::new(42);
    /// let v = vec![1, 2, 3];
    /// drop(b);
    /// drop(v);
    ///
    /// // Safety: memory allocated in blink mode won't be used after reset.
    /// unsafe {
    ///     GLOBAL_ALLOC.reset();
    ///     GLOBAL_ALLOC.direct_mode();
    /// };
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

        (*self.state.get()).blink.reset_unchecked();
    }

    /// Switches allocator to blink mode.
    /// All allocations will be served by blink-allocator.
    ///
    /// The type is created in direct mode.
    /// When used as global allocator, user may manually switch into blink mode
    /// in `main` or at any point later.
    ///
    /// However user must switch back to direct mode before returning from `main`.
    ///
    /// # Safety
    ///
    /// Must be externally synchronized with other threads accessing this allocator.
    /// Memory allocated in direct mode must not be deallocated while in blink mode.
    #[inline(always)]
    pub unsafe fn blink_mode(&self) {
        (*self.state.get()).enabled = true;
    }

    /// Switches allocator to blink mode.
    /// All allocations will be served by underlying allocator.
    ///
    /// The type is created in direct mode.
    /// When used as global allocator, user may manually switch into blink mode
    /// in `main` or at any point later.
    ///
    /// However user must switch back to direct mode before returning from `main`.
    ///
    /// # Safety
    ///
    /// Must be externally synchronized with other threads accessing this allocator.
    /// Memory allocated in blink mode must not be deallocated while in direct mode.
    #[inline(always)]
    pub unsafe fn direct_mode(&self) {
        self.reset();
        (*self.state.get()).enabled = false;
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
    ///
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
        unsafe { (*self.state.get()).blink.local() }
    }
}

unsafe impl<A> GlobalAlloc for GlobalBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline]
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        match (*self.state.get()).allocate(layout) {
            Ok(ptr) => {
                #[cfg(debug_assertions)]
                if (*self.state.get()).enabled {
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
        (*self.state.get()).deallocate(ptr, layout);
        #[cfg(debug_assertions)]
        {
            if (*self.state.get()).enabled {
                let _ = self.allocations.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: core::alloc::Layout) -> *mut u8 {
        match (*self.state.get()).allocate_zeroed(layout) {
            Ok(ptr) => {
                #[cfg(debug_assertions)]
                if (*self.state.get()).enabled {
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
            None => (*self.state.get()).allocate(new_layout),
            Some(ptr) => (*self.state.get()).resize(ptr, layout, new_layout),
        };

        match result {
            Ok(ptr) => ptr.as_ptr().cast(),
            Err(_) => null_mut(),
        }
    }
}
