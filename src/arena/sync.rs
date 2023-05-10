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
}
