use core::{
    alloc::{GlobalAlloc, Layout},
    cell::{Cell, UnsafeCell},
    ptr::{null_mut, NonNull},
};

use allocator_api2::alloc::{AllocError, Allocator};

use crate::{cold, local::BlinkAlloc, unreachable_unchecked};

enum State<A: Allocator> {
    Initialized(BlinkAlloc<A>),
    Uninitialized(A),
}

impl<A: Allocator> State<A> {
    #[inline(always)]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            State::Initialized(blink_alloc) => blink_alloc.allocate(layout),
            State::Uninitialized(allocator) => {
                cold();
                allocator.allocate(layout)
            }
        }
    }

    #[inline(always)]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            State::Initialized(blink_alloc) => blink_alloc.allocate_zeroed(layout),
            State::Uninitialized(allocator) => {
                cold();
                allocator.allocate_zeroed(layout)
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
        match self {
            State::Initialized(blink_alloc) => blink_alloc.resize(ptr, old_layout, new_layout),
            State::Uninitialized(allocator) => {
                cold();
                if old_layout.size() >= new_layout.size() {
                    allocator.grow(ptr, old_layout, new_layout)
                } else {
                    allocator.shrink(ptr, old_layout, new_layout)
                }
            }
        }
    }

    #[inline(always)]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        match self {
            State::Initialized(blink_alloc) => blink_alloc.deallocate(ptr, layout.size()),
            State::Uninitialized(allocator) => {
                cold();
                allocator.deallocate(ptr, layout)
            }
        }
    }
}

switch_std_default! {
    /// [`GlobalAlloc`] implementation based on [`BlinkAlloc`].
    pub struct UnsafeGlobalBlinkAlloc<A: Allocator = +std::alloc::System> {
        inner: UnsafeCell<State<A>>,
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
            inner: UnsafeCell::new(State::Uninitialized(allocator)),
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
    /// unsafe { GLOBAL_ALLOC.init() };
    ///
    /// let b = Box::new(42);
    /// let v = vec![1, 2, 3];
    /// drop(b);
    /// drop(v);
    ///
    /// // Safety: memory allocated after `init` call won't be used after reset.
    /// unsafe { GLOBAL_ALLOC.reset() };
    /// # }
    /// # #[cfg(not(feature = "std"))] fn main() {}
    /// ```
    #[inline(always)]
    pub unsafe fn reset(&self) {
        #[cfg(debug_assertions)]
        {
            assert_eq!(self.allocations.get(), 0, "Not everything was deallocated");
        }
        match *self.inner.get() {
            State::Initialized(ref mut allocator) => allocator.reset(),
            State::Uninitialized(_) => cold(),
        }
    }

    /// Initializes this allocator.
    /// Before this call any allocation will go directly to the underlying allocator.
    ///
    /// # Safety
    ///
    /// Must be called exactly once.
    /// Must be externally synchronized with other threads accessing this allocator.
    #[inline(always)]
    pub unsafe fn init(&self) {
        let alloc = match core::ptr::read(self.inner.get()) {
            State::Uninitialized(allocator) => BlinkAlloc::new_in(allocator),
            State::Initialized(_) => unreachable_unchecked(),
        };
        core::ptr::write(self.inner.get(), State::Initialized(alloc));

        #[cfg(debug_assertions)]
        {
            self.allocations.set(0);
        }
    }

    /// Initializes this allocator.
    /// With this method you can specify initial chunk size.
    /// Before this call any allocation will go directly to the underlying allocator.
    ///
    /// # Safety
    ///
    /// Must be called exactly once.
    /// Must be externally synchronized with other threads accessing this allocator.
    #[inline(always)]
    pub unsafe fn init_with_chunk_size(&self, chunk_size: usize) {
        let alloc = match core::ptr::read(self.inner.get()) {
            State::Uninitialized(allocator) => {
                BlinkAlloc::with_chunk_size_in(chunk_size, allocator)
            }
            State::Initialized(_) => unreachable_unchecked(),
        };
        core::ptr::write(self.inner.get(), State::Initialized(alloc));

        #[cfg(debug_assertions)]
        {
            self.allocations.set(0);
        }
    }
}

unsafe impl<A> GlobalAlloc for UnsafeGlobalBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline]
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        match (*self.inner.get()).allocate(layout) {
            Ok(ptr) => {
                #[cfg(debug_assertions)]
                {
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
        (*self.inner.get()).deallocate(ptr, layout);
        #[cfg(debug_assertions)]
        {
            self.allocations
                .set(self.allocations.get().saturating_sub(1));
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: core::alloc::Layout) -> *mut u8 {
        match (*self.inner.get()).allocate_zeroed(layout) {
            Ok(ptr) => {
                #[cfg(debug_assertions)]
                {
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
            None => (*self.inner.get()).allocate(new_layout),
            Some(ptr) => (*self.inner.get()).resize(ptr, layout, new_layout),
        };

        match result {
            Ok(ptr) => ptr.as_ptr().cast(),
            Err(_) => null_mut(),
        }
    }
}
