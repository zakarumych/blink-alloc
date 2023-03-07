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

/// 4 KB. After power-of-two threshold, new chunk size is aligned to this value.
const CHUNK_PAGE_SIZE_THRESHOLD: usize = 1 << 12;

/// 1/8 KB. Minimum chunk size growth step.
const CHUNK_MIN_GROW_STEP: usize = 64;

#[repr(C)]
pub struct ChunkHeader<T> {
    cursor: T,
    end: *mut u8,
    prev: Option<NonNull<Self>>,
    cumulative_size: usize,
}

impl<T> ChunkHeader<T>
where
    T: CasPtr,
{
    #[inline]
    unsafe fn alloc_chunk(
        size: usize,
        allocator: &impl Allocator,
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
        allocator: &impl Allocator,
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
    unsafe fn init_chunk(slice: NonNull<[u8]>, prev: Option<NonNull<Self>>) -> NonNull<Self> {
        let len = slice.len();
        let ptr = slice.as_ptr().cast::<u8>();
        debug_assert!(is_aligned_to(sptr::Strict::addr(ptr), align_of::<Self>()));
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
                cursor: T::new(base),
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

    // #[inline(always)]
    // fn base_mut(&mut self) -> *mut u8 {
    //     unsafe { <*mut Self>::add(self, 1).cast() }
    // }

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

    // /// # Safety
    // ///
    // /// `ptr` must be a pointer withing the usable memory of the chunk.
    // /// e.g. it must be between `base` and `self`.
    // #[inline(always)]
    // unsafe fn offset_from_base(&self, ptr: *const u8) -> usize {
    //     // Safety: end and base belong to the same memory chunk.
    //     let offset = unsafe { ptr.offset_from(self.base()) };
    //     offset as usize
    // }

    #[inline(always)]
    fn cap(&self) -> usize {
        // Safety: `base` fits `base..=self` range.
        unsafe { self.offset_from_end(self.base()) }
    }

    /// One round of allocation attempt.
    #[inline(always)]
    fn alloc_round(
        &self,
        cursor: *mut u8,
        layout: Layout,
        exchange: impl FnOnce(*mut u8) -> Result<(), *mut u8>,
    ) -> Option<Result<NonNull<[u8]>, *mut u8>> {
        let cursor_addr = sptr::Strict::addr(cursor);

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

        let end_addr = sptr::Strict::addr(self.end);
        if next_addr > end_addr {
            return None;
        }

        let aligned = unsafe { cursor.add(aligned_addr - cursor_addr) };
        let next = unsafe { aligned.add(layout.size()) };

        if let Err(updated) = exchange(next) {
            return Some(Err(updated));
        };

        // Actual allocation length.
        let len = next_addr - cursor_addr;
        debug_assert!(len >= layout.size());

        // Safety:
        // offset is within unused allocated memory range starting from base.
        // base is not null.
        unsafe {
            debug_assert_eq!(aligned_addr % layout.align(), 0);
            let slice = core::ptr::slice_from_raw_parts_mut(aligned, len);
            Some(Ok(NonNull::new_unchecked(slice)))
        }
    }

    // Safety: `chunk` must be a pointer to the valid chunk allocation.
    #[inline(always)]
    unsafe fn alloc<const ZEROED: bool>(
        chunk: NonNull<Self>,
        layout: Layout,
    ) -> Option<NonNull<[u8]>> {
        // Safety: `chunk` is a valid pointer to chunk allocation.
        let me = unsafe { chunk.as_ref() };
        let mut cursor = me.cursor.load(Ordering::Relaxed);

        loop {
            let result = me.alloc_round(cursor, layout, |aligned| {
                me.cursor.compare_exchange_weak(
                    cursor,
                    aligned,
                    Ordering::Acquire, // Memory access valid only *after* this succeeds.
                    Ordering::Relaxed,
                )
            })?;

            match result {
                Ok(slice) => {
                    if ZEROED {
                        unsafe { ptr::write_bytes(slice.as_ptr().cast::<u8>(), 0, slice.len()) }
                    }
                    return Some(slice);
                }
                Err(updated) => {
                    cold();
                    cursor = updated;
                }
            }
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
    unsafe fn resize<const ZEROED: bool>(
        chunk: NonNull<Self>,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Option<NonNull<[u8]>> {
        // Safety: `chunk` is a valid pointer to chunk allocation.
        let me = unsafe { chunk.as_ref() };

        let addr = sptr::Strict::addr(ptr.as_ptr());
        if old_layout.align() >= new_layout.align() {
            // || addr & (new_layout.align() - 1) == 0 {
            if new_layout.size() <= old_layout.size() {
                let slice = core::ptr::slice_from_raw_parts_mut(ptr.as_ptr(), new_layout.size());
                return Some(NonNull::new_unchecked(slice));
            } else {
                // Safety:
                // `ptr + old_layout.size()` is within allocation or one by past end.
                let old_end = unsafe { ptr.as_ptr().add(old_layout.size()) };

                let cursor = me.cursor.load(Ordering::Relaxed);
                if cursor == old_end {
                    let next_addr = addr.checked_add(new_layout.size())?;

                    let end_addr = sptr::Strict::addr(me.end);
                    if next_addr > end_addr {
                        // Not enough space.
                        return None;
                    }

                    let next = unsafe { ptr.as_ptr().add(new_layout.size()) };

                    let result = me.cursor.compare_exchange(
                        cursor,
                        next,
                        Ordering::Acquire, // Acquire more memory.
                        Ordering::Relaxed,
                    );

                    if let Ok(()) = result {
                        if ZEROED && old_layout.size() < new_layout.size() {
                            core::ptr::write_bytes(
                                ptr.as_ptr().add(old_layout.size()),
                                0,
                                new_layout.size() - old_layout.size(),
                            );
                        }

                        let slice =
                            core::ptr::slice_from_raw_parts_mut(ptr.as_ptr(), new_layout.size());
                        return Some(NonNull::new_unchecked(slice));
                    }
                    cold();
                }
            }
        } else {
            cold();
        }

        // if new_layout.size() <= old_size {
        //     if ALIGNED {
        //         let old_end = ptr.as_ptr().add(new_layout.size());
        //         let new_end = ptr.as_ptr().add(new_layout.size());
        //         if old_end != new_end {
        //             debug_assert!(old_end > new_end);
        //             // Free if possible.
        //             let _ = me.cursor.compare_exchange(
        //                 old_end,
        //                 new_end,
        //                 Ordering::Release, // Released some memory.
        //                 Ordering::Relaxed,
        //             );
        //         }

        //         let slice = core::ptr::slice_from_raw_parts_mut(ptr.as_ptr(), new_layout.size());
        //         return Ok(NonNull::new_unchecked(slice));
        //     } else {
        //         // Try to shrink-shift.
        //         if let Some(aligned_addr) = align_up(addr, new_layout.align()) {
        //             let max_shift = old_size - new_layout.size();
        //             if addr + max_shift >= aligned_addr {
        //                 // Now fits.
        //                 let aligned = ptr.as_ptr().add(aligned_addr - addr);

        //                 memmove(ptr.as_ptr(), aligned, new_layout.size());

        //                 let new_end = aligned.add(new_layout.size());

        //                 if old_end != new_end {
        //                     debug_assert!(old_end > new_end);
        //                     // Free if possible.
        //                     let _ = me.cursor.compare_exchange(
        //                         old_end,
        //                         new_end,
        //                         Ordering::Release, // Released some memory.
        //                         Ordering::Relaxed,
        //                     );
        //                 }

        //                 let slice = core::ptr::slice_from_raw_parts_mut(aligned, new_layout.size());
        //                 return Ok(NonNull::new_unchecked(slice));
        //             }
        //         }
        //     }
        // }

        // let cursor = me.cursor.load(Ordering::Relaxed);
        // if cursor == old_end {
        //     // Possible to grow-shift.

        //     let unfit = || {
        //         // Safety:
        //         // `ptr` is always within `base..=self` range.
        //         let used = unsafe { me.offset_from_base(ptr.as_ptr()) };

        //         // Find size that will fit previous allocation and the new one.
        //         let next_size = layout_sum(&new_layout).checked_add(used);

        //         // Minimal grow step.
        //         let min_grow = me.cap().checked_add(CHUNK_MIN_GROW_STEP);

        //         // Returns the bigger one the two.
        //         next_size.max(min_grow)
        //     };

        //     let aligned_addr;
        //     let next_addr;

        //     if ALIGNED {
        //         let Some(next) = addr.checked_add(new_layout.size()) else {
        //             // Impossible to grow or reallocate.
        //             return Err(unfit());
        //         };
        //         next_addr = next;
        //         aligned_addr = addr;
        //     } else {
        //         let Some(unaligned) = addr.checked_add(layout_sum(&new_layout)) else {
        //             // Impossible to grow or reallocate.
        //             return Err(unfit());
        //         };
        //         aligned_addr = align_down(unaligned - new_layout.size(), new_layout.align());
        //         next_addr = aligned_addr + new_layout.size();
        //     };

        //     debug_assert!(
        //         aligned_addr >= addr,
        //         "aligned_addr must not be less than addr"
        //     );
        //     debug_assert!(
        //         (aligned_addr - addr) < new_layout.align(),
        //         "Cannot waste space more than alignment size"
        //     );

        //     let end_addr = sptr::Strict::addr(me.end);
        //     if next_addr > end_addr {
        //         // Not enough space.
        //         return Err((next_addr - end_addr).checked_add(me.cap()));
        //     }

        //     let cursor_addr = sptr::Strict::addr(cursor);
        //     let aligned = unsafe { cursor.offset(aligned_addr as isize - cursor_addr as isize) };
        //     let next = unsafe { aligned.add(new_layout.size()) };

        //     let result = me.cursor.compare_exchange(
        //         old_end,
        //         next,
        //         Ordering::Acquire, // Acquire more memory.
        //         Ordering::Relaxed,
        //     );

        //     if let Ok(()) = result {
        //         // Move bytes from old location to new.
        //         // Use smaller size of the old and new allocation.
        //         memmove(ptr.as_ptr(), aligned, new_layout.size().min(old_size));

        //         if ZEROED && old_size < new_layout.size() {
        //             core::ptr::write_bytes(aligned.add(old_size), 0, new_layout.size() - old_size);
        //         }

        //         let slice = core::ptr::slice_from_raw_parts_mut(aligned, new_layout.size());
        //         return Ok(NonNull::new_unchecked(slice));
        //     }

        //     cold();
        // }

        // Have to reallocate.
        let new_ptr = ChunkHeader::alloc::<false>(chunk, new_layout)?;

        // Copy bytes from old location to new.
        // Separate allocations cannot overlap.
        core::ptr::copy_nonoverlapping(
            ptr.as_ptr(),
            new_ptr.as_ptr().cast(),
            new_layout.size().min(old_layout.size()),
        );

        if ZEROED && old_layout.size() < new_layout.size() {
            core::ptr::write_bytes(
                new_ptr.as_ptr().cast::<u8>(),
                0,
                new_layout.size() - old_layout.size(),
            );
        }

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

#[inline(always)]
pub unsafe fn alloc_fast<T, const ZEROED: bool>(
    root: Option<NonNull<ChunkHeader<T>>>,
    layout: Layout,
) -> Option<NonNull<[u8]>>
where
    T: CasPtr,
{
    let root = root?;
    // Safety: `chunk` is a valid pointer to chunk allocation.
    unsafe { ChunkHeader::alloc::<ZEROED>(root, layout) }
}

#[cold]
pub unsafe fn alloc_slow<T, A, const ZEROED: bool>(
    root: &Cell<Option<NonNull<ChunkHeader<T>>>>,
    mut chunk_size: usize,
    layout: Layout,
    allocator: &A,
) -> Result<NonNull<[u8]>, AllocError>
where
    T: CasPtr,
    A: Allocator,
{
    if let Some(root) = root.get() {
        chunk_size = chunk_size.max(root.as_ref().cumulative_size);
        chunk_size = chunk_size
            .checked_add(layout.size().max(CHUNK_MIN_GROW_STEP))
            .ok_or(AllocError)?;
    } else {
        chunk_size = chunk_size.max(layout.size());
    }

    if layout.align() > align_of::<ChunkHeader<T>>() {
        chunk_size = chunk_size.checked_add(layout.align()).ok_or(AllocError)?;
    }

    let Some(mut chunk_size) = chunk_size.checked_add(size_of::<ChunkHeader<T>>()) else {
        return Err(AllocError);
    };

    // Grow size exponentially until a threshold.
    if chunk_size < CHUNK_POWER_OF_TWO_THRESHOLD {
        chunk_size = chunk_size.next_power_of_two();
    } else {
        chunk_size = align_up(chunk_size, CHUNK_PAGE_SIZE_THRESHOLD).unwrap_or(chunk_size);
    }

    debug_assert_eq!(chunk_size % align_of::<ChunkHeader<T>>(), 0);
    let new_chunk = ChunkHeader::alloc_chunk(chunk_size, allocator, root.get())?;

    // Safety: `chunk` is a valid pointer to chunk allocation.
    let ptr = unsafe { ChunkHeader::alloc::<ZEROED>(new_chunk, layout).unwrap_unchecked() };

    root.set(Some(new_chunk));
    Ok(ptr)
}

#[inline(always)]
pub unsafe fn resize_fast<T, const ZEROED: bool>(
    root: Option<NonNull<ChunkHeader<T>>>,
    ptr: NonNull<u8>,
    old_layout: Layout,
    new_layout: Layout,
) -> Option<NonNull<[u8]>>
where
    T: CasPtr,
{
    let root = root?;

    // Safety: `chunk` is a valid pointer to chunk allocation.
    unsafe { ChunkHeader::resize::<ZEROED>(root, ptr, old_layout, new_layout) }
}

#[cold]
pub unsafe fn resize_slow<T, A, const ZEROED: bool>(
    root: &Cell<Option<NonNull<ChunkHeader<T>>>>,
    chunk_size: usize,
    ptr: NonNull<u8>,
    old_layout: Layout,
    new_layout: Layout,
    allocator: &A,
) -> Result<NonNull<[u8]>, AllocError>
where
    T: CasPtr,
    A: Allocator,
{
    let new_ptr = alloc_slow::<_, _, false>(root, chunk_size, new_layout, allocator)?;
    core::ptr::copy_nonoverlapping(
        ptr.as_ptr(),
        new_ptr.as_ptr().cast(),
        new_layout.size().min(old_layout.size()),
    );
    if ZEROED && old_layout.size() < new_layout.size() {
        core::ptr::write_bytes(
            ptr.as_ptr().add(old_layout.size()),
            0,
            new_layout.size() - old_layout.size(),
        );
    }
    // Deallocation is impossible.
    Ok(new_ptr)
}

#[inline(always)]
pub unsafe fn dealloc<T>(root: Option<NonNull<ChunkHeader<T>>>, ptr: NonNull<u8>, size: usize)
where
    T: CasPtr,
{
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
pub unsafe fn reset<T, A>(
    root: &Cell<Option<NonNull<ChunkHeader<T>>>>,
    keep_last: bool,
    allocator: &A,
) where
    T: CasPtr,
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
        prev = unsafe { ChunkHeader::dealloc_chunk(chunk, allocator) };
    }
}

#[inline(always)]
pub fn reset_leak<T>(root: &Cell<Option<NonNull<ChunkHeader<T>>>>, keep_last: bool)
where
    T: CasPtr,
{
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

pub trait Arena {
    /// Allocates memory with `layout` from this arena.
    /// Uses `allocator` to allocate new chunks.
    unsafe fn alloc<const ZEROED: bool>(
        &self,
        layout: Layout,
        allocator: &impl Allocator,
    ) -> Result<NonNull<[u8]>, AllocError>;

    /// Attempts to resize memory block previously allocated with `Arena`.
    /// If possible shrinks or grows in place.
    unsafe fn resize<const ZEROED: bool>(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
        allocator: &impl Allocator,
    ) -> Result<NonNull<[u8]>, AllocError>;

    /// Deallocates memory that was previously allocated with `alloc`.
    /// If `ptr` points to the very last allocation, bumps cursor back.
    /// Otherwise does nothing.
    unsafe fn dealloc(&self, ptr: NonNull<u8>, size: usize);

    /// Reset this arena, invalidating all previous allocations.
    /// Chunks are deallocated with `allocator`.
    /// If `keep_last` is `true`, the last chunk will be kept and reused.
    unsafe fn reset(&mut self, keep_last: bool, allocator: &impl Allocator);

    /// Reset this arena, invalidating all previous allocations.
    /// Chunks are deallocated with `allocator`.
    /// If `keep_last` is `true`, the last chunk will be kept and reused.
    unsafe fn reset_unchecked(&self, keep_last: bool, allocator: &impl Allocator);

    /// Reset internals by leaking all chunks.
    /// Useful for cases where leaked memory will be reclaimed
    /// by the allocator.
    /// If `keep_last` is `true`, the last chunk will be kept and reused.
    fn reset_leak(&mut self, keep_last: bool);

    /// Reset internals by leaking all chunks.
    /// Useful for cases where leaked memory will be reclaimed
    /// by the allocator.
    /// If `keep_last` is `true`, the last chunk will be kept and reused.
    unsafe fn reset_leak_unchecked(&self, keep_last: bool);
}

/// Thread-local arena allocator.
pub struct ArenaLocal {
    root: Cell<Option<NonNull<ChunkHeader<Cell<*mut u8>>>>>,
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
}

impl Arena for ArenaLocal {
    #[inline(always)]
    unsafe fn alloc<const ZEROED: bool>(
        &self,
        layout: Layout,
        allocator: &impl Allocator,
    ) -> Result<NonNull<[u8]>, AllocError> {
        match alloc_fast::<_, ZEROED>(self.root.get(), layout) {
            Some(ptr) => Ok(ptr),
            None => {
                alloc_slow::<_, _, ZEROED>(&self.root, self.min_chunk_size.get(), layout, allocator)
            }
        }
    }

    #[inline(always)]
    unsafe fn resize<const ZEROED: bool>(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
        allocator: &impl Allocator,
    ) -> Result<NonNull<[u8]>, AllocError> {
        match resize_fast::<_, ZEROED>(self.root.get(), ptr, old_layout, new_layout) {
            Some(ptr) => Ok(ptr),
            None => resize_slow::<_, _, ZEROED>(
                &self.root,
                self.min_chunk_size.get(),
                ptr,
                old_layout,
                new_layout,
                allocator,
            ),
        }
    }

    #[inline(always)]
    unsafe fn dealloc(&self, ptr: NonNull<u8>, size: usize) {
        dealloc(self.root.get(), ptr, size)
    }

    #[inline(always)]
    unsafe fn reset(&mut self, keep_last: bool, allocator: &impl Allocator) {
        unsafe { reset(&self.root, keep_last, allocator) }
    }

    #[inline(always)]
    unsafe fn reset_unchecked(&self, keep_last: bool, allocator: &impl Allocator) {
        unsafe { reset(&self.root, keep_last, allocator) }
    }

    #[inline(always)]
    fn reset_leak(&mut self, keep_last: bool) {
        reset_leak(&self.root, keep_last)
    }

    #[inline(always)]
    unsafe fn reset_leak_unchecked(&self, keep_last: bool) {
        reset_leak(&self.root, keep_last)
    }
}

#[cfg(feature = "sync")]
mod sync {
    use super::*;

    struct Inner {
        root: Option<NonNull<ChunkHeader<Cell<*mut u8>>>>,
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
    }

    impl Arena for ArenaSync {
        #[inline(always)]
        unsafe fn alloc<const ZEROED: bool>(
            &self,
            layout: Layout,
            allocator: &impl Allocator,
        ) -> Result<NonNull<[u8]>, AllocError> {
            let inner = self.inner.read();

            match alloc_fast::<_, ZEROED>(inner.root, layout) {
                Some(ptr) => Ok(ptr),
                None => {
                    drop(inner);
                    let mut guard = self.inner.write();
                    let inner = &mut *guard;

                    alloc_slow::<_, _, ZEROED>(
                        Cell::from_mut(&mut inner.root),
                        inner.min_chunk_size,
                        layout,
                        allocator,
                    )
                }
            }
        }

        #[inline(always)]
        unsafe fn resize<const ZEROED: bool>(
            &self,
            ptr: NonNull<u8>,
            old_layout: Layout,
            new_layout: Layout,
            allocator: &impl Allocator,
        ) -> Result<NonNull<[u8]>, AllocError> {
            let inner = self.inner.read();
            match resize_fast::<_, ZEROED>(inner.root, ptr, old_layout, new_layout) {
                Some(ptr) => Ok(ptr),
                None => {
                    drop(inner);
                    let mut guard = self.inner.write();
                    let inner = &mut *guard;

                    resize_slow::<_, _, ZEROED>(
                        Cell::from_mut(&mut inner.root),
                        inner.min_chunk_size,
                        ptr,
                        old_layout,
                        new_layout,
                        allocator,
                    )
                }
            }
        }

        #[inline(always)]
        unsafe fn dealloc(&self, ptr: NonNull<u8>, size: usize) {
            dealloc(self.inner.read().root, ptr, size)
        }

        #[inline(always)]
        unsafe fn reset(&mut self, keep_last: bool, allocator: &impl Allocator) {
            unsafe {
                reset(
                    Cell::from_mut(&mut self.inner.get_mut().root),
                    keep_last,
                    allocator,
                )
            }
        }

        #[inline(always)]
        unsafe fn reset_unchecked(&self, keep_last: bool, allocator: &impl Allocator) {
            let mut guard = self.inner.write();
            unsafe { reset(Cell::from_mut(&mut guard.root), keep_last, allocator) }
        }

        #[inline(always)]
        fn reset_leak(&mut self, keep_last: bool) {
            reset_leak(Cell::from_mut(&mut self.inner.get_mut().root), keep_last)
        }

        #[inline(always)]
        unsafe fn reset_leak_unchecked(&self, keep_last: bool) {
            let mut guard = self.inner.write();
            reset_leak(Cell::from_mut(&mut guard.root), keep_last)
        }
    }
}

use crate::cold;

#[cfg(feature = "sync")]
pub use self::sync::ArenaSync;

// #[inline(always)]
// unsafe fn memmove(src: *mut u8, dst: *mut u8, size: usize) {
//     if src == dst {
//         return;
//     }

//     #[cold]
//     #[inline(always)]
//     unsafe fn cold_copy(src: *mut u8, dst: *mut u8, size: usize) {
//         core::ptr::copy(src, dst, size);
//     }

//     cold_copy(src, dst, size)
// }
