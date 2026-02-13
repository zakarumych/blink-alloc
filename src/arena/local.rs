use super::*;

with_cursor!(Cell<*mut u8>);

/// Thread-local arena allocator.
pub struct ArenaLocal {
    root: Cell<Option<NonNull<ChunkHeader>>>,
    min_chunk_size: Cell<usize>,
}

/// It is safe to send `ArenaLocal` between threads.
unsafe impl Send for ArenaLocal {}

impl Drop for ArenaLocal {
    #[inline(always)]
    fn drop(&mut self) {
        debug_assert!(
            self.root.get().is_none(),
            "Owner must reset `ArenaLocal` with `keep_last` set to `false` before drop"
        );
    }
}

impl ArenaLocal {
    #[inline(always)]
    pub const fn new() -> Self {
        ArenaLocal {
            root: Cell::new(None),
            min_chunk_size: Cell::new(CHUNK_START_SIZE),
        }
    }

    #[inline(always)]
    pub const fn with_chunk_size(min_chunk_size: usize) -> Self {
        ArenaLocal {
            root: Cell::new(None),
            min_chunk_size: Cell::new(min_chunk_size),
        }
    }

    #[inline(always)]
    #[cfg(feature = "sync")]
    pub fn last_chunk_size(&self) -> usize {
        match self.root.get() {
            None => 0,
            Some(root) => {
                // Safety: `root` is a valid pointer to chunk allocation.
                unsafe { root.as_ref().cap() }
            }
        }
    }

    #[inline(always)]
    pub unsafe fn alloc_fast(&self, layout: Layout) -> Option<NonNull<[u8]>> {
        if let Some(root) = self.root.get() {
            return unsafe { ChunkHeader::alloc(root, layout) };
        }
        None
    }

    #[inline(always)]
    pub unsafe fn alloc_slow(
        &self,
        layout: Layout,
        allocator: impl Allocator,
    ) -> Result<NonNull<[u8]>, AllocError> {
        alloc_slow(&self.root, self.min_chunk_size.get(), layout, allocator)
    }

    #[inline(always)]
    pub unsafe fn resize_fast(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Option<NonNull<[u8]>> {
        if let Some(root) = self.root.get() {
            return unsafe { ChunkHeader::resize(root, ptr, old_layout, new_layout) };
        }
        None
    }

    #[inline(always)]
    pub unsafe fn resize_slow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
        allocator: impl Allocator,
    ) -> Result<NonNull<[u8]>, AllocError> {
        resize_slow(
            &self.root,
            self.min_chunk_size.get(),
            ptr,
            old_layout,
            new_layout,
            allocator,
        )
    }

    #[inline(always)]
    pub unsafe fn dealloc(&self, ptr: NonNull<u8>, size: usize) {
        dealloc(self.root.get(), ptr, size)
    }

    #[inline(always)]
    pub unsafe fn reset(&mut self, keep_last: bool, allocator: impl Allocator) {
        unsafe { reset(&self.root, keep_last, allocator) }
    }

    #[inline(always)]
    pub unsafe fn reset_unchecked(&self, keep_last: bool, allocator: impl Allocator) {
        unsafe { reset(&self.root, keep_last, allocator) }
    }

    #[cfg(feature = "sync")]
    #[inline(always)]
    pub fn reset_leak(&mut self, keep_last: bool) {
        reset_leak(&self.root, keep_last)
    }

    /// Returns the approximate number of bytes allocated from this arena.
    ///
    /// This is computed by summing the capacity of all previous chunks
    /// (which are ~fully used, minus alignment padding) plus the cursor
    /// offset in the current chunk. After warm-up (when a single chunk
    /// serves all allocations), this is exact.
    pub fn allocated_bytes(&self) -> usize {
        let Some(root) = self.root.get() else {
            return 0;
        };
        let chunk = unsafe { root.as_ref() };
        let cursor = chunk.cursor.load(Ordering::Relaxed) as usize;
        let base = chunk.base() as usize;
        let current_used = cursor - base;
        current_used + chunk.cumulative_size
    }

    /// Returns the total capacity of all chunks in this arena.
    pub fn total_capacity(&self) -> usize {
        let Some(root) = self.root.get() else {
            return 0;
        };
        let chunk = unsafe { root.as_ref() };
        chunk.cap() + chunk.cumulative_size
    }
}
