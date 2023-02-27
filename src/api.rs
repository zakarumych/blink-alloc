use allocator_api2::Allocator;

/// Extension trait for [`Allocator`] that defines blink allocator API.
/// Blink-allocators are allocators with cheap allocation
/// and potentially no-op deallocation.
/// Blink-allocator *can* reuse deallocated memory, but not required to.
/// Typically deallocation is either no-op or processed only if deallocating
/// the very last allocated memory block.
/// The [`reset`][BlinkAllocator::reset]
/// method deallocates all memory allocated from this instance at once.
pub unsafe trait BlinkAllocator: Allocator {
    /// Resets allocator potentially invalidating all allocations
    /// made from this instance.
    /// This is no-op if allocator is [`Clone`][core::clone::Clone]
    /// (typically shared reference to blink-allocator).
    ///
    /// # Safety
    ///
    /// Caller must guarantee that all allocations made from this instance
    /// won't be used after this call.
    /// The potential invalid memory access will happen through
    /// raw pointer and requires `unsafe`.
    ///
    /// This method requires mutable borrow of the blink-allocator
    /// and so cannot be called while collection types reference it.
    /// So it is safe to use reference to blink allocator as allocator type
    /// for, say, [`Vec`] and later then call `reset` safely.
    /// The `Vec` would have to be dropped (or forgotten) before calling `reset`
    /// due to mutable borrow.
    ///
    /// [`Vec`]: alloc::vec::Vec
    fn reset(&mut self);
}

unsafe impl<A> BlinkAllocator for &A
where
    A: BlinkAllocator,
{
    #[inline]
    fn reset(&mut self) {}
}

unsafe impl<'a, A> BlinkAllocator for &'a mut A
where
    &'a mut A: Allocator,
    A: BlinkAllocator,
{
    #[inline]
    fn reset(&mut self) {
        A::reset(self);
    }
}
