use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    ptr::{null_mut, NonNull},
};

#[cfg(debug_assertions)]
use core::cell::Cell;

use allocator_api2::alloc::{AllocError, Allocator};

use crate::{cold, local::BlinkAlloc};

struct State<A: Allocator> {
    blink: BlinkAlloc<A>,
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
    /// [`GlobalAlloc`] implementation based on [`BlinkAlloc`].
    pub struct UnsafeGlobalBlinkAlloc<A: Allocator = +std::alloc::System> {
        state: UnsafeCell<State<A>>,
        #[cfg(debug_assertions)]
        allocations: Cell<u64>,
    }
}

// The user is responsible for ensuring that this allocator
// won't be used concurrently.
// To make this sound, `UnsafeGlobalBlinkAlloc::new` and `UnsafeGlobalBlinkAlloc::new_in`
// are marked unsafe.
unsafe impl<A: Allocator + Send> Send for UnsafeGlobalBlinkAlloc<A> {}
unsafe impl<A: Allocator + Sync> Sync for UnsafeGlobalBlinkAlloc<A> {}

#[cfg(feature = "std")]
impl UnsafeGlobalBlinkAlloc<std::alloc::System> {
    /// Create a new [`UnsafeGlobalBlinkAlloc`].
    ///
    /// Const function can be used to initialize a static variable.
    ///
    /// # Safety
    ///
    /// This method is unsafe because this type is not thread-safe
    /// but implements `Sync`.
    /// Allocator returned by this method must not be used concurrently.
    ///
    /// For safer alternative see [`GlobalBlinkAlloc`](https://docs.rs/blink-alloc/0.2.2/blink_alloc/struct.GlobalBlinkAlloc.html).
    ///
    /// # Example
    ///
    /// ```
    /// use blink_alloc::UnsafeGlobalBlinkAlloc;
    ///
    /// // Safety: This program is single-threaded.
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: UnsafeGlobalBlinkAlloc = unsafe { UnsafeGlobalBlinkAlloc::new() };
    ///
    /// let _ = Box::new(42);
    /// let _ = vec![1, 2, 3];
    /// ```
    pub const unsafe fn new() -> Self {
        UnsafeGlobalBlinkAlloc::new_in(std::alloc::System)
    }

    /// Create a new [`UnsafeGlobalBlinkAlloc`].
    ///
    /// This method allows to specify initial chunk size.
    ///
    /// Const function can be used to initialize a static variable.
    ///
    /// # Safety
    ///
    /// This method is unsafe because this type is not thread-safe
    /// but implements `Sync`.
    /// Allocator returned by this method must not be used concurrently.
    ///
    /// For safer alternative see [`GlobalBlinkAlloc`](https://docs.rs/blink-alloc/0.2.2/blink_alloc/struct.GlobalBlinkAlloc.html).
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "std")] fn main() {
    /// use blink_alloc::UnsafeGlobalBlinkAlloc;
    ///
    /// // Safety: This program is single-threaded.
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: UnsafeGlobalBlinkAlloc<std::alloc::System> = unsafe { UnsafeGlobalBlinkAlloc::new_in(std::alloc::System) };
    ///
    /// let _ = Box::new(42);
    /// let _ = vec![1, 2, 3];
    /// # }
    /// # #[cfg(not(feature = "std"))] fn main() {}
    /// ```
    pub const unsafe fn with_chunk_size(chunk_size: usize) -> Self {
        UnsafeGlobalBlinkAlloc::with_chunk_size_in(chunk_size, std::alloc::System)
    }
}

impl<A> UnsafeGlobalBlinkAlloc<A>
where
    A: Allocator,
{
    /// Create a new [`UnsafeGlobalBlinkAlloc`]
    /// with specified underlying allocator.
    ///
    /// Const function can be used to initialize a static variable.
    ///
    /// # Safety
    ///
    /// This method is unsafe because this type is not thread-safe
    /// but implements `Sync`.
    /// Allocator returned by this method must not be used concurrently.
    ///
    /// For safer alternative see [`GlobalBlinkAlloc`](https://docs.rs/blink-alloc/0.2.2/blink_alloc/struct.GlobalBlinkAlloc.html).
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "std")] fn main() {
    /// use blink_alloc::UnsafeGlobalBlinkAlloc;
    ///
    /// // Safety: This program is single-threaded.
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: UnsafeGlobalBlinkAlloc<std::alloc::System> = unsafe { UnsafeGlobalBlinkAlloc::new_in(std::alloc::System) };
    ///
    /// let _ = Box::new(42);
    /// let _ = vec![1, 2, 3];
    /// # }
    /// # #[cfg(not(feature = "std"))] fn main() {}
    /// ```
    pub const unsafe fn new_in(allocator: A) -> Self {
        UnsafeGlobalBlinkAlloc {
            state: UnsafeCell::new(State {
                blink: BlinkAlloc::new_in(allocator),
                enabled: false,
            }),
            #[cfg(debug_assertions)]
            allocations: Cell::new(0),
        }
    }

    /// Create a new [`UnsafeGlobalBlinkAlloc`]
    /// with specified underlying allocator.
    ///
    /// This method allows to specify initial chunk size.
    ///
    /// Const function can be used to initialize a static variable.
    ///
    /// # Safety
    ///
    /// This method is unsafe because this type is not thread-safe
    /// but implements `Sync`.
    /// Allocator returned by this method must not be used concurrently.
    ///
    /// For safer alternative see [`GlobalBlinkAlloc`](https://docs.rs/blink-alloc/0.2.2/blink_alloc/struct.GlobalBlinkAlloc.html).
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "std")] fn main() {
    /// use blink_alloc::UnsafeGlobalBlinkAlloc;
    ///
    /// // Safety: This program is single-threaded.
    /// #[global_allocator]
    /// static GLOBAL_ALLOC: UnsafeGlobalBlinkAlloc<std::alloc::System> = unsafe { UnsafeGlobalBlinkAlloc::new_in(std::alloc::System) };
    ///
    /// let _ = Box::new(42);
    /// let _ = vec![1, 2, 3];
    /// # }
    /// # #[cfg(not(feature = "std"))] fn main() {}
    /// ```
    pub const unsafe fn with_chunk_size_in(chunk_size: usize, allocator: A) -> Self {
        UnsafeGlobalBlinkAlloc {
            state: UnsafeCell::new(State {
                blink: BlinkAlloc::with_chunk_size_in(chunk_size, allocator),
                enabled: false,
            }),
            #[cfg(debug_assertions)]
            allocations: Cell::new(0),
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
            assert_eq!(self.allocations.get(), 0, "Not everything was deallocated");
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
}

unsafe impl<A> GlobalAlloc for UnsafeGlobalBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline]
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        match (*self.state.get()).allocate(layout) {
            Ok(ptr) => {
                #[cfg(debug_assertions)]
                if (*self.state.get()).enabled {
                    self.allocations.set(self.allocations.get() + 1);
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
        if (*self.state.get()).enabled {
            self.allocations
                .set(self.allocations.get().saturating_sub(1));
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: core::alloc::Layout) -> *mut u8 {
        match (*self.state.get()).allocate_zeroed(layout) {
            Ok(ptr) => {
                #[cfg(debug_assertions)]
                if (*self.state.get()).enabled {
                    self.allocations.set(self.allocations.get() + 1);
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
