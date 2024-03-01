//! This module provides `ArenaLocal` and `ArenaSync`
//! types which share implementation
//! but use `Cell` and `RwLock` + `AtomicUsize` respectively.
//! for the interior mutability.

use core::{
    alloc::Layout,
    cell::Cell,
    mem::{align_of, size_of},
    ptr::{self, NonNull},
    sync::atomic::{AtomicPtr, Ordering},
};

use allocator_api2::alloc::{AllocError, Allocator};

use crate::cold;

#[cfg(feature = "sync")]
use parking_lot::RwLock;

#[inline(always)]
fn is_aligned_to(value: usize, align: usize) -> bool {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    value & mask == 0
}

#[inline(always)]
fn align_up(value: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    Some(value.checked_add(mask)? & !mask)
}

#[inline(always)]
fn align_down(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    value & !mask
}

/// A sum of layout size and align mask.
#[inline(always)]
fn layout_sum(layout: &Layout) -> usize {
    // Layout constrains guarantee that this won't overflow.
    layout.size() + (layout.align() - 1)
}

pub trait CasPtr {
    fn new(value: *mut u8) -> Self;
    fn load(&self, order: Ordering) -> *mut u8;
    fn set(&mut self, value: *mut u8);
    fn compare_exchange(
        &self,
        old: *mut u8,
        new: *mut u8,
        success: Ordering,
        failure: Ordering,
    ) -> Result<(), *mut u8>;

    fn compare_exchange_weak(
        &self,
        old: *mut u8,
        new: *mut u8,
        success: Ordering,
        failure: Ordering,
    ) -> Result<(), *mut u8>;
}

impl CasPtr for Cell<*mut u8> {
    #[inline(always)]
    fn new(value: *mut u8) -> Self {
        Cell::new(value)
    }

    #[inline(always)]
    fn load(&self, _: Ordering) -> *mut u8 {
        self.get()
    }

    #[inline(always)]
    fn set(&mut self, value: *mut u8) {
        *self.get_mut() = value;
    }

    #[inline(always)]
    fn compare_exchange(
        &self,
        old: *mut u8,
        new: *mut u8,
        _: Ordering,
        _: Ordering,
    ) -> Result<(), *mut u8> {
        if old == self.get() {
            self.set(new);
            Ok(())
        } else {
            Err(self.get())
        }
    }

    #[inline(always)]
    fn compare_exchange_weak(
        &self,
        old: *mut u8,
        new: *mut u8,
        _: Ordering,
        _: Ordering,
    ) -> Result<(), *mut u8> {
        debug_assert_eq!(
            old,
            self.get(),
            "Must be used only in loop where `old` is last loaded value"
        );
        self.set(new);
        Ok(())
    }
}

impl CasPtr for AtomicPtr<u8> {
    #[inline(always)]
    fn new(value: *mut u8) -> Self {
        AtomicPtr::new(value)
    }

    #[inline(always)]
    fn load(&self, order: Ordering) -> *mut u8 {
        self.load(order)
    }

    #[inline(always)]
    fn set(&mut self, value: *mut u8) {
        *self.get_mut() = value;
    }

    #[inline(always)]
    fn compare_exchange(
        &self,
        old: *mut u8,
        new: *mut u8,
        success: Ordering,
        failure: Ordering,
    ) -> Result<(), *mut u8> {
        self.compare_exchange(old, new, success, failure)?;
        Ok(())
    }

    #[inline(always)]
    fn compare_exchange_weak(
        &self,
        old: *mut u8,
        new: *mut u8,
        success: Ordering,
        failure: Ordering,
    ) -> Result<(), *mut u8> {
        self.compare_exchange_weak(old, new, success, failure)?;
        Ok(())
    }
}

/// 0.25 KB. Initial chunk size.
const CHUNK_START_SIZE: usize = 256;

/// 16 KB. After this size, new chunk size is not aligned to next power of two.
const CHUNK_POWER_OF_TWO_THRESHOLD: usize = 1 << 14;

/// 1/16 KB. Minimum chunk size growth step.
const CHUNK_MIN_GROW_STEP: usize = 64;

macro_rules! with_cursor {
    ($cursor:ty) => {
        #[repr(C)]
        pub struct ChunkHeader {
            cursor: $cursor,
            end: *mut u8,
            prev: Option<NonNull<Self>>,
            cumulative_size: usize,
        }

        impl ChunkHeader {
            #[inline]
            unsafe fn alloc_chunk(
                size: usize,
                allocator: impl Allocator,
                prev: Option<NonNull<Self>>,
            ) -> Result<NonNull<Self>, AllocError> {
                let Some(size) = align_up(size, align_of::<Self>()) else {
                    return Err(AllocError);
                };

                // Safety:
                // size + (align - 1) hasn't overflow above.
                // `align_of` returns valid align value.
                let layout = unsafe { Layout::from_size_align_unchecked(size, align_of::<Self>()) };
                let slice = allocator.allocate(layout)?;
                Ok(Self::init_chunk(slice, prev))
            }

            #[inline]
            unsafe fn dealloc_chunk(
                chunk: NonNull<Self>,
                allocator: impl Allocator,
            ) -> Option<NonNull<Self>> {
                let me = unsafe { chunk.as_ref() };
                let prev = me.prev;

                let size = unsafe { me.end.offset_from(chunk.as_ptr().cast()) } as usize;

                // Safety:
                // Making layout of actual allocation.
                let layout = unsafe { Layout::from_size_align_unchecked(size, align_of::<Self>()) };

                allocator.deallocate(chunk.cast(), layout);
                prev
            }

            /// # Safety
            ///
            /// `ptr` must be a pointer to the valid chunk allocation.
            /// `ptr` must be aligned for `ChunkHeader` structure.
            /// `size` must be the size of the allocation.
            /// `size` must be large enough to fit `Chunk` structure.
            #[inline]
            unsafe fn init_chunk(
                slice: NonNull<[u8]>,
                prev: Option<NonNull<Self>>,
            ) -> NonNull<Self> {
                let len = slice.len();
                let ptr = slice.as_ptr().cast::<u8>();
                debug_assert!(is_aligned_to(ptr as usize, align_of::<Self>()));
                debug_assert!(len > size_of::<Self>());

                let end = ptr.add(len);

                let header_ptr = ptr.cast::<Self>();
                let base = header_ptr.add(1).cast::<u8>();

                let cumulative_size = match prev {
                    None => 0,
                    Some(prev) => {
                        let prev = unsafe { prev.as_ref() };
                        prev.cap() + prev.cumulative_size
                    }
                };

                ptr::write(
                    header_ptr,
                    ChunkHeader {
                        cursor: <$cursor>::new(base),
                        end,
                        prev,
                        cumulative_size,
                    },
                );
                NonNull::new_unchecked(header_ptr)
            }

            #[inline(always)]
            fn base(&self) -> *const u8 {
                unsafe { <*const Self>::add(self, 1).cast() }
            }

            /// # Safety
            ///
            /// `ptr` must be a pointer withing the usable memory of the chunk.
            /// e.g. it must be between `base` and `self`.
            #[inline(always)]
            unsafe fn offset_from_end(&self, ptr: *const u8) -> usize {
                // Safety: end and base belong to the same memory chunk.
                let offset = unsafe { self.end.offset_from(ptr) };
                offset as usize
            }

            #[inline(always)]
            fn cap(&self) -> usize {
                // Safety: `base` fits `base..=self` range.
                unsafe { self.offset_from_end(self.base()) }
            }

            // Safety: `chunk` must be a pointer to the valid chunk allocation.
            #[inline(always)]
            unsafe fn alloc(chunk: NonNull<Self>, layout: Layout) -> Option<NonNull<[u8]>> {
                // Safety: `chunk` is a valid pointer to chunk allocation.
                let me = unsafe { chunk.as_ref() };
                let mut cursor = me.cursor.load(Ordering::Relaxed);

                loop {
                    let cursor_addr = cursor as usize;

                    let layout_sum = layout_sum(&layout);

                    let unaligned = cursor_addr.checked_add(layout_sum)?;

                    let aligned_addr = align_down(unaligned - layout.size(), layout.align());

                    debug_assert!(
                        aligned_addr >= cursor_addr,
                        "aligned_addr addr must not be less than cursor"
                    );
                    debug_assert!(
                        (aligned_addr - cursor_addr) < layout.align(),
                        "Cannot waste space more than alignment size"
                    );

                    let next_addr = aligned_addr + layout.size();

                    let end_addr = me.end as usize;
                    if next_addr > end_addr {
                        return None;
                    }

                    let aligned = unsafe { cursor.add(aligned_addr - cursor_addr) };
                    let next = unsafe { aligned.add(layout.size()) };

                    if let Err(updated) = me.cursor.compare_exchange_weak(
                        cursor,
                        next,
                        Ordering::Acquire, // Memory access valid only *after* this succeeds.
                        Ordering::Relaxed,
                    ) {
                        cursor = updated;
                        continue;
                    };

                    // Actual allocation length.
                    let len = next_addr - aligned_addr;
                    debug_assert!(len >= layout.size());

                    // Safety:
                    // offset is within unused allocated memory range starting from base.
                    // base is not null.
                    let slice = unsafe {
                        debug_assert_eq!(aligned_addr % layout.align(), 0);
                        let slice = core::ptr::slice_from_raw_parts_mut(aligned, len);
                        NonNull::new_unchecked(slice)
                    };

                    return Some(slice);
                }
            }

            /// Optimistic resize for arena-allocated memory.
            /// Handles grows, shrinks if new alignment requirement is not met - shifts.
            /// When alignment requirement is already met (checked for pointer itself)
            /// shifts do not happen for both shrinks and grows.
            /// Even more, cheap shrinks are always successful if alignment is met by `ptr`.
            /// Cheap grows are successful if this is the last allocation in the chunk
            /// and there is enough space for the new allocation.
            /// If cheap shrink or grow is not possible - reallocates.
            ///
            /// Safety: `chunk` must be a pointer to the valid chunk allocation.
            /// `ptr` must be a pointer to the allocated memory of at least `old_size` bytes.
            /// `ptr` may be allocated from different chunk.
            #[inline]
            unsafe fn resize(
                chunk: NonNull<Self>,
                ptr: NonNull<u8>,
                old_layout: Layout,
                new_layout: Layout,
            ) -> Option<NonNull<[u8]>> {
                // Safety: `chunk` is a valid pointer to chunk allocation.
                let me = unsafe { chunk.as_ref() };

                let addr = ptr.as_ptr() as usize;
                if old_layout.align() >= new_layout.align() {
                    if new_layout.size() <= old_layout.size() {
                        let slice =
                            core::ptr::slice_from_raw_parts_mut(ptr.as_ptr(), old_layout.size());
                        return Some(NonNull::new_unchecked(slice));
                    } else {
                        // Safety:
                        // `ptr + old_layout.size()` is within allocation or one by past end.
                        let old_end = unsafe { ptr.as_ptr().add(old_layout.size()) };

                        let cursor = me.cursor.load(Ordering::Relaxed);
                        if cursor == old_end {
                            let next_addr = addr.checked_add(new_layout.size())?;

                            let end_addr = me.end as usize;
                            if next_addr > end_addr {
                                // Not enough space.
                                return None;
                            }

                            let next = unsafe { ptr.as_ptr().add(new_layout.size()) };

                            let result = CasPtr::compare_exchange(
                                &me.cursor,
                                cursor,
                                next,
                                Ordering::Acquire, // Acquire more memory.
                                Ordering::Relaxed,
                            );

                            if let Ok(()) = result {
                                let len = next_addr - addr;
                                debug_assert!(len >= new_layout.size());

                                let slice = core::ptr::slice_from_raw_parts_mut(ptr.as_ptr(), len);
                                return Some(NonNull::new_unchecked(slice));
                            }
                            cold();
                        }
                    }
                } else {
                    cold();
                }

                // Have to reallocate.
                let new_ptr = ChunkHeader::alloc(chunk, new_layout)?;

                // Copy bytes from old location to new.
                // Separate allocations cannot overlap.
                core::ptr::copy_nonoverlapping(
                    ptr.as_ptr(),
                    new_ptr.as_ptr().cast(),
                    new_layout.size().min(old_layout.size()),
                );

                // Deallocation is not possible.
                Some(new_ptr)
            }

            // Safety: `chunk` must be a pointer to the valid chunk allocation.
            #[inline(always)]
            unsafe fn reset(mut chunk: NonNull<Self>) -> Option<NonNull<Self>> {
                let me = chunk.as_mut();
                let base = me.end.sub(me.cap());
                me.cursor.set(base);
                me.cumulative_size = 0;
                me.prev.take()
            }

            // Safety: `chunk` must be a pointer to the valid chunk allocation.
            // `ptr` must be a pointer to the allocated memory of at least `size` bytes.
            // `ptr` may be allocated from different chunk.
            #[inline(always)]
            unsafe fn dealloc(chunk: NonNull<Self>, ptr: NonNull<u8>, size: usize) {
                // Safety: `chunk` is a valid pointer to chunk allocation.
                let me = unsafe { chunk.as_ref() };

                // Safety: `ptr` is a valid pointer to the allocated memory of at least `size` bytes.
                let new = unsafe { ptr.as_ptr().add(size) };

                // Single attempt to update cursor.
                // Fails if `ptr` is not the last memory allocated from this chunk.
                // Spurious failures in multithreaded environment are possible
                // but do not affect correctness.
                let _ = me.cursor.compare_exchange(
                    ptr.as_ptr(),
                    new,
                    Ordering::Release, // Released some memory.
                    Ordering::Relaxed,
                );
            }
        }

        #[cold]
        pub unsafe fn alloc_slow(
            root: &Cell<Option<NonNull<ChunkHeader>>>,
            mut chunk_size: usize,
            layout: Layout,
            allocator: impl Allocator,
        ) -> Result<NonNull<[u8]>, AllocError> {
            if let Some(root) = root.get() {
                chunk_size = chunk_size.max(root.as_ref().cumulative_size);
                chunk_size = chunk_size
                    .checked_add(layout.size().max(CHUNK_MIN_GROW_STEP))
                    .ok_or(AllocError)?;
            } else {
                chunk_size = chunk_size.max(layout.size());
            }

            if layout.align() > align_of::<ChunkHeader>() {
                chunk_size = chunk_size.checked_add(layout.align()).ok_or(AllocError)?;
            }

            let Some(mut chunk_size) = chunk_size.checked_add(size_of::<ChunkHeader>()) else {
                return Err(AllocError);
            };

            // Grow size exponentially until a threshold.
            if chunk_size < CHUNK_POWER_OF_TWO_THRESHOLD {
                chunk_size = chunk_size.next_power_of_two();
            } else {
                chunk_size =
                    align_up(chunk_size, CHUNK_POWER_OF_TWO_THRESHOLD).unwrap_or(chunk_size);
            }

            debug_assert_eq!(chunk_size % align_of::<ChunkHeader>(), 0);
            let new_chunk = ChunkHeader::alloc_chunk(chunk_size, allocator, root.get())?;

            // Safety: `chunk` is a valid pointer to chunk allocation.
            let ptr = unsafe { ChunkHeader::alloc(new_chunk, layout).unwrap_unchecked() };

            root.set(Some(new_chunk));
            Ok(ptr)
        }

        #[cold]
        pub unsafe fn resize_slow(
            root: &Cell<Option<NonNull<ChunkHeader>>>,
            chunk_size: usize,
            ptr: NonNull<u8>,
            old_layout: Layout,
            new_layout: Layout,
            allocator: impl Allocator,
        ) -> Result<NonNull<[u8]>, AllocError> {
            let new_ptr = alloc_slow(root, chunk_size, new_layout, allocator)?;
            core::ptr::copy_nonoverlapping(
                ptr.as_ptr(),
                new_ptr.as_ptr().cast(),
                new_layout.size().min(old_layout.size()),
            );
            // Deallocation is impossible.
            Ok(new_ptr)
        }

        #[inline(always)]
        pub unsafe fn dealloc(root: Option<NonNull<ChunkHeader>>, ptr: NonNull<u8>, size: usize) {
            if let Some(root) = root {
                // Safety:
                // `chunk` is a valid pointer to chunk allocation.
                // `ptr` is a valid pointer to the allocated memory of at least `size` bytes.
                unsafe {
                    ChunkHeader::dealloc(root, ptr, size);
                }
            }
        }

        /// Safety:
        /// `allocator` must be the same allocator that was used in `alloc`.
        #[inline(always)]
        pub unsafe fn reset<A>(
            root: &Cell<Option<NonNull<ChunkHeader>>>,
            keep_last: bool,
            allocator: A,
        ) where
            A: Allocator,
        {
            let mut prev = if keep_last {
                let Some(root) = root.get() else {
                    return;
                };

                // Safety: `chunk` is a valid pointer to chunk allocation.
                // This function owns mutable reference to `self`.
                unsafe { ChunkHeader::reset(root) }
            } else {
                root.take()
            };

            while let Some(chunk) = prev {
                // Safety: `chunk` is a valid pointer to chunk allocation.
                // Allocated from this allocator with this layout.
                prev = unsafe { ChunkHeader::dealloc_chunk(chunk, &allocator) };
            }
        }

        #[allow(dead_code)]
        #[inline(always)]
        pub fn reset_leak(root: &Cell<Option<NonNull<ChunkHeader>>>, keep_last: bool) {
            if keep_last {
                let Some(chunk) = root.get() else {
                    return;
                };

                // Safety: `chunk` is a valid pointer to chunk allocation.
                // This function owns mutable reference to `self`.
                unsafe {
                    ChunkHeader::reset(chunk);
                }
            } else {
                root.set(None);
            };
        }
    };
}

mod local;
pub use self::local::ArenaLocal;

#[cfg(feature = "sync")]
mod sync;

#[cfg(feature = "sync")]
pub use self::sync::ArenaSync;
