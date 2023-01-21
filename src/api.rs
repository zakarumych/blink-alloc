pub use self::inner::{AllocError, Allocator};

#[cfg(feature = "alloc")]
pub use self::inner::Global;

#[cfg(not(feature = "nightly"))]
mod inner {
    #[cfg(feature = "alloc")]
    use crate::alloc::alloc::{alloc, dealloc};

    use core::{alloc::Layout, ptr::NonNull};

    /// Mimics `core::alloc::AllocError` struct.
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    pub struct AllocError;

    /// Mimics `core::alloc::Allocator` trait.
    pub unsafe trait Allocator {
        fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, AllocError>;
        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout);
    }

    unsafe impl<A> Allocator for &A
    where
        A: Allocator,
    {
        #[inline(always)]
        fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
            A::allocate(*self, layout)
        }

        #[inline(always)]
        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
            A::deallocate(self, ptr, layout)
        }
    }

    /// Mimics `alloc::alloc::Global` struct.
    #[cfg(feature = "alloc")]
    #[derive(Copy, Clone)]
    pub struct Global;

    #[cfg(feature = "alloc")]
    unsafe impl Allocator for Global {
        #[inline]
        fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
            unsafe { NonNull::new(alloc(layout)).ok_or(AllocError) }
        }

        #[inline]
        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
            dealloc(ptr.as_ptr(), layout);
        }
    }

    #[cfg(feature = "alloc")]
    impl Default for Global {
        #[inline]
        fn default() -> Self {
            Global
        }
    }
}

#[cfg(feature = "nightly")]
mod inner {
    pub use core::alloc::{AllocError, Allocator};

    #[cfg(feature = "alloc")]
    pub use alloc::alloc::Global;
}

/// Trait that defines blink allocator API.
pub unsafe trait BlinkAllocator: Allocator {
    /// Resets allocator potentially invalidating all allocations
    /// made from this instance.
    /// This is no-op if allocator instance is [`Clone`][core::clone::Clone].
    unsafe fn reset(&mut self);
}

unsafe impl<A> BlinkAllocator for &A
where
    A: BlinkAllocator,
{
    #[inline(always)]
    unsafe fn reset(&mut self) {}
}

unsafe impl<A> BlinkAllocator for &mut A
where
    A: BlinkAllocator,
    for<'a> &'a mut A: Allocator,
{
    #[inline(always)]
    unsafe fn reset(&mut self) {
        A::reset(self)
    }
}
