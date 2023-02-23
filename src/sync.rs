//! This module provides single-threaded blink allocator.

use core::{
    alloc::Layout,
    hint::unreachable_unchecked,
    mem::{align_of, size_of},
    ptr::{self, NonNull},
    sync::atomic::{AtomicUsize, Ordering},
};

use parking_lot::RwLock;

use crate::{
    align::align_up,
    api::{AllocError, Allocator, BlinkAllocator, Global},
    cold,
};

/// 4 KB. Initial chunk size.
const CHUNK_START_SIZE: usize = 4096;

/// 32 MB. After this size, new chunk size is not aligned to next power of two.
const CHUNK_POWER_OF_TWO_THRESHOLD: usize = 1 << 25;

/// 4 KB. Alignment for chunks larger than `CHUNK_POWER_OF_TWO_THRESHOLD`.
const CHUNK_MIN_ALIGN: usize = 4096;

#[repr(C)]
#[repr(align(16))]
struct Chunk {
    used: AtomicUsize,
    cap: usize,
    prev: Option<NonNull<Chunk>>,
}

impl Chunk {
    fn _alloc(&self, base: *mut u8, layout: Layout) -> Result<NonNull<[u8]>, usize> {
        // This does not overflow due to `Layout` constraints
        let padded_size = layout.size() + (layout.align() - 1);

        if self.cap < padded_size {
            return Err(self.cap + padded_size);
        }

        let offset = self.used.fetch_add(padded_size, Ordering::Relaxed);
        if offset > self.cap - padded_size {
            self.used.fetch_sub(padded_size, Ordering::Relaxed);
            return Err(offset + padded_size);
        }

        // Safety:
        // offset is within unused allocated memory range starting from base.
        // Aligned pointer address is not smaller than unaligned.
        // base is not null.
        unsafe {
            let ptr = base.add(offset);
            let addr = sptr::Strict::addr(ptr);
            let aligned = align_up(addr, layout.align()).unwrap_unchecked();
            let add = aligned - addr;
            let ptr = ptr.add(add);
            let size = padded_size - add;
            let slice = core::ptr::slice_from_raw_parts_mut(ptr, size);
            Ok(NonNull::new_unchecked(slice))
        }
    }

    // Safety: `chunk` must be a pointer to the valid chunk allocation.
    #[inline(always)]
    unsafe fn alloc(chunk: NonNull<Self>, layout: Layout) -> Result<NonNull<[u8]>, usize> {
        // Safety: `chunk` is a valid pointer to `Chunk` memory allocation.
        let base = unsafe { chunk.as_ptr().add(1).cast::<u8>() };
        chunk.as_ref()._alloc(base, layout)
    }
}

struct Inner {
    root: Option<NonNull<Chunk>>,
    last_chunk_size: usize,
}

unsafe impl Send for Inner {}

/// Multi-threaded blink allocator.
/// Uses lock-free algorithm to serve memory allocations.
pub struct SyncBlinkAlloc<A: Allocator> {
    inner: RwLock<Inner>,
    allocator: A,
}

impl<A> Default for SyncBlinkAlloc<A>
where
    A: Allocator + Default,
{
    #[inline(always)]
    fn default() -> Self {
        Self::new_in(Default::default())
    }
}

#[cfg(feature = "alloc")]
impl SyncBlinkAlloc<Global> {
    /// Creates new blink allocator that uses global allocator
    /// to allocate memory chunks.
    ///
    /// See [`SyncBlinkAlloc::new_in`] for using custom allocator.
    #[inline(always)]
    pub fn new() -> Self {
        SyncBlinkAlloc::new_in(Global)
    }
}

impl<A> SyncBlinkAlloc<A>
where
    A: Allocator,
{
    /// Creates new blink allocator that uses provided allocator
    /// to allocate memory chunks.
    ///
    /// See [`SyncBlinkAlloc::new`] for using global allocator.
    #[inline(always)]
    pub fn new_in(allocator: A) -> Self {
        SyncBlinkAlloc {
            inner: RwLock::new(Inner {
                root: None,
                last_chunk_size: 0,
            }),
            allocator,
        }
    }

    /// Main allocation method.
    /// All other allocation methods are implemented in terms of this one.
    fn _alloc(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let Some(mut min_chunk_size) = layout.size().checked_add(layout.align()).and_then(|l| l.checked_add(size_of::<Chunk>())) else {
            // Layout is too large to fit into a chunk
            return Err(AllocError);
        };

        loop {
            let inner = self.inner.read();

            if let Some(chunk) = inner.root {
                // Safety: `chunk` is a valid pointer to chunk allocation.
                let res = unsafe { Chunk::alloc(chunk, layout) };
                match res {
                    Ok(ptr) => return Ok(ptr),
                    Err(min_size) => {
                        min_chunk_size = min_chunk_size.max(min_size);
                    }
                }
            }

            // Have to allocate new chunk.
            cold();

            let old_root = inner.root;

            drop(inner);
            let mut inner = self.inner.write();
            if let Some(root) = inner.root {
                if old_root != Some(root) {
                    // Another thread allocated a chunk.
                    continue;
                }
            }

            let mut chunk_size = inner.last_chunk_size.saturating_add(min_chunk_size);
            if chunk_size < CHUNK_POWER_OF_TWO_THRESHOLD {
                chunk_size = chunk_size.next_power_of_two();
            } else {
                chunk_size = align_up(chunk_size, CHUNK_MIN_ALIGN).unwrap_or(chunk_size);
            };
            chunk_size = chunk_size.max(CHUNK_START_SIZE);

            let Ok(chunk_layout) = Layout::from_size_align(chunk_size, align_of::<Chunk>()) else {
                // Failed to construct chunk layout.
                return Err(AllocError);
            };

            let chunk_ptr = self.allocator.allocate(chunk_layout)?;
            let new_chunk = chunk_ptr.cast::<Chunk>();

            // Safety: `chunk_ptr` is a valid pointer to chunk allocation.
            unsafe {
                ptr::write(
                    new_chunk.as_ptr(),
                    Chunk {
                        used: AtomicUsize::new(0),
                        cap: chunk_size - size_of::<Chunk>(),
                        prev: inner.root,
                    },
                );
            }

            inner.root = Some(new_chunk);
        }
    }

    /// Resets this allocator, deallocating all chunks except the last one.
    /// Last chunk will be reused.
    /// With steady memory usage after few iterations
    /// chunk size should be sufficient for all allocations between resets.
    pub fn reset(&mut self) {
        self._reset(true);
    }

    fn _reset(&mut self, keep_last: bool) {
        let inner = self.inner.get_mut();

        let mut next = if keep_last {
            let Some(mut chunk) = inner.root else {
                return;
            };

            // Safety: `chunk` is a valid pointer to chunk allocation.
            // This function owns mutable reference to `self`.
            unsafe {
                let chunk = chunk.as_mut();
                *chunk.used.get_mut() = 0;
                chunk.prev.take()
            }
        } else {
            inner.root.take()
        };

        while let Some(chunk) = next {
            let chunk_layout = {
                // Safety: `chunk` is a valid pointer to chunk allocation.
                let chunk = unsafe { chunk.as_ref() };
                next = chunk.prev;

                let chunk_size = chunk.cap + size_of::<Chunk>();

                let Ok(chunk_layout) = Layout::from_size_align(chunk_size, align_of::<Chunk>()) else {
                    // Safety: chunk was allocated with this layout.
                    unsafe { unreachable_unchecked() }
                };

                chunk_layout
            };

            // Safety: `chunk` is a valid pointer to chunk allocation.
            // Allocated from this allocator with this layout.
            unsafe { self.allocator.deallocate(chunk.cast(), chunk_layout) }
        }
    }
}

impl<A> Drop for SyncBlinkAlloc<A>
where
    A: Allocator,
{
    fn drop(&mut self) {
        self._reset(false);
    }
}

unsafe impl<A> Allocator for SyncBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline(always)]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self._alloc(layout)
    }

    #[inline(always)]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let _ = (ptr, layout);
    }
}

unsafe impl<A> Allocator for &mut SyncBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline(always)]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self._alloc(layout)
    }

    #[inline(always)]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let _ = (ptr, layout);
    }
}

unsafe impl<A> BlinkAllocator for SyncBlinkAlloc<A>
where
    A: Allocator,
{
    #[inline(always)]
    unsafe fn reset(&mut self) {
        self._reset(true)
    }
}
