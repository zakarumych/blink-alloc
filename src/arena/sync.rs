use super::*;

with_cursor!(AtomicPtr<u8>);

struct Inner {
    root: Option<NonNull<ChunkHeader>>,
    min_chunk_size: usize,
}

unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

/// Multi-threaded arena allocator.
pub struct ArenaSync {
    inner: RwLock<Inner>,
}

impl Drop for ArenaSync {
    #[inline(always)]
    fn drop(&mut self) {
        debug_assert!(
            self.inner.get_mut().root.is_none(),
            "Owner must reset `ArenaSync` with `keep_last` set to `false` before drop"
        );
    }
}

impl ArenaSync {
    #[inline(always)]
    pub const fn new() -> Self {
        ArenaSync {
            inner: RwLock::new(Inner {
                root: None,
                min_chunk_size: CHUNK_START_SIZE,
            }),
        }
    }

    #[inline(always)]
    pub const fn with_chunk_size(min_chunk_size: usize) -> Self {
        ArenaSync {
            inner: RwLock::new(Inner {
                root: None,
                min_chunk_size,
            }),
        }
    }

    #[inline(always)]
    pub unsafe fn alloc_fast(&self, layout: Layout) -> Option<NonNull<[u8]>> {
        let inner = self.inner.read();

        if let Some(root) = inner.root {
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
        let mut guard = self.inner.write();
        let inner = &mut *guard;

        alloc_slow(
            Cell::from_mut(&mut inner.root),
            inner.min_chunk_size,
            layout,
            &allocator,
        )
    }

    #[inline(always)]
    pub unsafe fn resize_fast(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Option<NonNull<[u8]>> {
        let inner = self.inner.read();

        if let Some(root) = inner.root {
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
        let mut guard = self.inner.write();
        let inner = &mut *guard;

        resize_slow(
            Cell::from_mut(&mut inner.root),
            inner.min_chunk_size,
            ptr,
            old_layout,
            new_layout,
            &allocator,
        )
    }

    #[inline(always)]
    pub unsafe fn dealloc(&self, ptr: NonNull<u8>, size: usize) {
        dealloc(self.inner.read().root, ptr, size)
    }

    #[inline(always)]
    pub unsafe fn reset(&mut self, keep_last: bool, allocator: impl Allocator) {
        unsafe {
            reset(
                Cell::from_mut(&mut self.inner.get_mut().root),
                keep_last,
                allocator,
            )
        }
    }

    #[inline(always)]
    pub unsafe fn reset_unchecked(&self, keep_last: bool, allocator: impl Allocator) {
        let mut guard = self.inner.write();
        unsafe { reset(Cell::from_mut(&mut guard.root), keep_last, allocator) }
    }

    // #[inline(always)]
    // pub fn reset_leak(&mut self, keep_last: bool) {
    //     reset_leak(Cell::from_mut(&mut self.inner.get_mut().root), keep_last)
    // }

    /// Returns the approximate number of bytes allocated from this arena.
    ///
    /// This is computed by summing the capacity of all previous chunks
    /// (which are ~fully used, minus alignment padding) plus the cursor
    /// offset in the current chunk. After warm-up (when a single chunk
    /// serves all allocations), this is exact.
    pub fn allocated_bytes(&self) -> usize {
        let inner = self.inner.read();
        let Some(root) = inner.root else {
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
        let inner = self.inner.read();
        let Some(root) = inner.root else {
            return 0;
        };
        let chunk = unsafe { root.as_ref() };
        chunk.cap() + chunk.cumulative_size
    }
}
