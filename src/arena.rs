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

use allocator_api2::{AllocError, Allocator};

#[cfg(feature = "sync")]
use parking_lot::RwLock;

#[inline]
fn is_aligned_to(value: usize, align: usize) -> bool {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    value & mask == 0
}

#[inline]
fn align_up(value: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    Some(value.checked_add(mask)? & !mask)
}

#[inline]
fn align_down(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    value & !mask
}

/// A sum of layout size and align mask.
#[inline]
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
    #[inline]
    fn new(value: *mut u8) -> Self {
        Cell::new(value)
    }

    #[inline]
    fn load(&self, _: Ordering) -> *mut u8 {
        self.get()
    }

    #[inline]
    fn set(&mut self, value: *mut u8) {
        *self.get_mut() = value;
    }

    #[inline]
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

    #[inline]
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
    #[inline]
    fn new(value: *mut u8) -> Self {
        AtomicPtr::new(value)
    }

    #[inline]
    fn load(&self, order: Ordering) -> *mut u8 {
        self.load(order)
    }

    #[inline]
    fn set(&mut self, value: *mut u8) {
        *self.get_mut() = value;
    }

    #[inline]
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

    #[inline]
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

trait Mut<T> {
    fn get(&self) -> T;
    fn set(&mut self, value: T);
}

impl<T: Copy> Mut<T> for &mut T {
    #[inline]
    fn get(&self) -> T {
        **self
    }

    #[inline]
    fn set(&mut self, value: T) {
        **self = value;
    }
}

impl<T: Copy> Mut<T> for &Cell<T> {
    #[inline]
    fn get(&self) -> T {
        Cell::get(*self)
    }

    #[inline]
    fn set(&mut self, value: T) {
        Cell::set(*self, value);
    }
}

/// 0.5 KB. Initial chunk size.
const CHUNK_START_SIZE: usize = 256;

/// 64 KB. After this size, new chunk size is not aligned to next power of two.
const CHUNK_POWER_OF_TWO_THRESHOLD: usize = 1 << 12;

/// 1/8 KB. Minimum chunk size growth step.
const CHUNK_MIN_GROW_STEP: usize = 64;

#[repr(C)]
pub struct ChunkHeader<T> {
    cursor: T,
    end: *mut u8,
    prev: Option<NonNull<Self>>,
}

impl<T> ChunkHeader<T>
where
    T: CasPtr,
{
    #[cold]
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

    #[cold]
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
    unsafe fn init_chunk(slice: NonNull<[u8]>, prev: Option<NonNull<Self>>) -> NonNull<Self> {
        let len = slice.len();
        let header_ptr = slice.as_ptr().cast::<u8>();
        debug_assert!(is_aligned_to(
            sptr::Strict::addr(header_ptr),
            align_of::<Self>()
        ));
        debug_assert!(len > size_of::<Self>());

        let end = header_ptr.add(len);

        let header_ptr = header_ptr.cast::<Self>();
        let base = header_ptr.add(1).cast::<u8>();

        ptr::write(
            header_ptr,
            ChunkHeader {
                cursor: T::new(base),
                end,
                prev,
            },
        );
        NonNull::new_unchecked(header_ptr)
    }

    #[inline]
    fn base(&self) -> *const u8 {
        unsafe { <*const Self>::add(self, 1).cast() }
    }

    /// # Safety
    ///
    /// `ptr` must be a pointer withing the usable memory of the chunk.
    /// e.g. it must be between `base` and `self`.
    #[inline]
    unsafe fn offset_from_end(&self, ptr: *const u8) -> usize {
        // Safety: end and base belong to the same memory chunk.
        let offset = unsafe { self.end.offset_from(ptr) };
        offset as usize
    }

    /// # Safety
    ///
    /// `ptr` must be a pointer withing the usable memory of the chunk.
    /// e.g. it must be between `base` and `self`.
    #[inline]
    unsafe fn offset_from_base(&self, ptr: *const u8) -> usize {
        // Safety: end and base belong to the same memory chunk.
        let offset = unsafe { ptr.offset_from(self.base()) };
        offset as usize
    }

    #[inline]
    fn cap(&self) -> usize {
        // Safety: `base` fits `base..=self` range.
        unsafe { self.offset_from_end(self.base()) }
    }

    /// One round of allocation attempt.
    #[inline]
    fn alloc_round(
        &self,
        cursor: *mut u8,
        layout: Layout,
        exchange: impl FnOnce(*mut u8) -> Result<(), *mut u8>,
    ) -> Result<Result<NonNull<[u8]>, *mut u8>, Option<usize>> {
        let cursor_addr = sptr::Strict::addr(cursor);

        let layout_sum = layout_sum(&layout);

        let Some(unaligned) = cursor_addr.checked_add(layout_sum) else {
            // Safety:
            // `cursor` is always within `base..=self` range.
            let used = unsafe { self.offset_from_base(cursor) };

            // Find size that will fit previous allocation and the new one.
            let Some(next_size) = layout_sum.checked_add(used)  else {
                return Err(None);
            };

            // Minimal grow step.
            let Some(min_grow) = self.cap().checked_add(CHUNK_MIN_GROW_STEP) else {
                return Err(None);
            };

            // Returns the bigger one the two.
            return Err(Some(next_size.max(min_grow)));
        };

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
            // Not enough space.
            return Err((next_addr - end_addr).checked_add(self.cap()));
        }

        let aligned = unsafe { cursor.add(aligned_addr - cursor_addr) };
        let next = unsafe { aligned.add(layout.size()) };

        if let Err(updated) = exchange(next) {
            return Ok(Err(updated));
        };

        // Actual allocation length.
        let len = next_addr - cursor_addr;
        debug_assert!(len >= layout.size());

        // Safety:
        // offset is within unused allocated memory range starting from base.
        // base is not null.
        unsafe {
            let slice = core::ptr::slice_from_raw_parts_mut(cursor, len);
            Ok(Ok(NonNull::new_unchecked(slice)))
        }
    }

    // Safety: `chunk` must be a pointer to the valid chunk allocation.
    #[inline]
    unsafe fn alloc(chunk: NonNull<Self>, layout: Layout) -> Result<NonNull<[u8]>, Option<usize>> {
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
            });

            match result {
                Ok(Ok(slice)) => return Ok(slice),
                Ok(Err(updated)) => cursor = updated,
                Err(next_size) => return Err(next_size),
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
    unsafe fn resize(
        chunk: NonNull<Self>,
        ptr: NonNull<u8>,
        old_size: usize,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, Option<usize>> {
        // Safety: `chunk` is a valid pointer to chunk allocation.
        let me = unsafe { chunk.as_ref() };

        // Safety:
        // `ptr + old_size` is within allocation or one by past end.
        let old_end = unsafe { ptr.as_ptr().add(old_size) };

        let addr = sptr::Strict::addr(ptr.as_ptr());

        if new_layout.size() <= old_size {
            // Try to shrink-shift.
            if let Some(aligned_addr) = align_up(addr, new_layout.align()) {
                let max_shift = old_size - new_layout.size();
                if addr + max_shift >= aligned_addr {
                    // Now fits.
                    let aligned = ptr.as_ptr().add(aligned_addr - addr);

                    memmove(ptr.as_ptr(), aligned, new_layout.size());

                    // Free if possible.
                    let _ = me.cursor.compare_exchange(
                        old_end,
                        aligned.add(new_layout.size()),
                        Ordering::Release, // Released some memory.
                        Ordering::Relaxed,
                    );

                    let slice = core::ptr::slice_from_raw_parts_mut(aligned, new_layout.size());
                    return Ok(NonNull::new_unchecked(slice));
                }
            }
        }

        if me.cursor.load(Ordering::Relaxed) == old_end {
            // Possible to grow-shift.

            let Some(unaligned) = addr.checked_add(layout_sum(&new_layout)) else {
                // Impossible to grow or reallocate.

                // Safety:
                // `ptr` is always within `base..=self` range.
                let used = unsafe { me.offset_from_base(ptr.as_ptr()) };

                // Find size that will fit previous allocation and the new one.
                let next_size = layout_sum(&new_layout).checked_add(used);

                // Minimal grow step.
                let min_grow = me.cap().checked_add(CHUNK_MIN_GROW_STEP);

                // Returns the bigger one the two.
                return Err(next_size.max(min_grow));
            };

            let aligned_addr = align_down(unaligned - new_layout.size(), new_layout.align());

            debug_assert!(
                aligned_addr >= addr,
                "aligned_addr addr must not be less than cursor"
            );
            debug_assert!(
                (aligned_addr - addr) < new_layout.align(),
                "Cannot waste space more than alignment size"
            );

            let next_addr = aligned_addr + new_layout.size();

            let end_addr = sptr::Strict::addr(me.end);
            if next_addr > end_addr {
                // Not enough space.
                return Err((next_addr - end_addr).checked_add(me.cap()));
            }

            let aligned = unsafe { ptr.as_ptr().add(aligned_addr - addr) };
            let next = unsafe { aligned.add(new_layout.size()) };

            let result = me.cursor.compare_exchange(
                old_end,
                next,
                Ordering::Acquire, // Acquire more memory.
                Ordering::Relaxed,
            );

            if let Ok(()) = result {
                // Move bytes from old location to new.
                // Use smaller size of the old and new allocation.
                memmove(ptr.as_ptr(), aligned, new_layout.size().min(old_size));

                let slice = core::ptr::slice_from_raw_parts_mut(aligned, new_layout.size());
                return Ok(NonNull::new_unchecked(slice));
            }
        }

        // Have to reallocate.
        let new_ptr = ChunkHeader::alloc(chunk, new_layout)?;

        // Copy bytes from old location to new.
        // Separate allocations cannot overlap.
        core::ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr().cast(), new_layout.size());

        // Deallocation is not possible.

        Ok(new_ptr)
    }

    // Safety: `chunk` must be a pointer to the valid chunk allocation.
    #[inline]
    unsafe fn reset(mut chunk: NonNull<Self>) -> Option<NonNull<Self>> {
        let me = chunk.as_mut();
        me.cursor.set(me.end);
        me.prev.take()
    }

    // Safety: `chunk` must be a pointer to the valid chunk allocation.
    // `ptr` must be a pointer to the allocated memory of at least `size` bytes.
    // `ptr` may be allocated from different chunk.
    #[inline]
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

#[inline]
unsafe fn alloc_fast<T>(
    root: Option<NonNull<ChunkHeader<T>>>,
    layout: Layout,
) -> Result<NonNull<[u8]>, Option<usize>>
where
    T: CasPtr,
{
    match root {
        Some(root) => {
            // Safety: `chunk` is a valid pointer to chunk allocation.
            unsafe { ChunkHeader::alloc(root, layout) }
        }
        None => {
            let Some(min_chunk_size) = layout_sum(&layout).checked_add(size_of::<ChunkHeader<T>>()) else {
                // Layout is too large to fit into a chunk
                return Err(None);
            };
            Err(Some(min_chunk_size))
        }
    }
}

#[cold]
unsafe fn alloc_slow<T, A>(
    mut root: impl Mut<Option<NonNull<ChunkHeader<T>>>>,
    mut chunk_size: usize,
    layout: Layout,
    allocator: &A,
) -> Result<NonNull<[u8]>, AllocError>
where
    T: CasPtr,
    A: Allocator,
{
    chunk_size += size_of::<ChunkHeader<T>>();

    // Grow size exponentially until a threshold.
    if chunk_size < CHUNK_POWER_OF_TWO_THRESHOLD {
        chunk_size = chunk_size.next_power_of_two();
    }

    let min_chunk_size = CHUNK_START_SIZE.max(size_of::<[ChunkHeader<T>; 16]>());
    chunk_size = chunk_size.max(min_chunk_size);

    let new_chunk = ChunkHeader::alloc_chunk(chunk_size, allocator, root.get())?;

    // Safety: `chunk` is a valid pointer to chunk allocation.
    let res = unsafe { ChunkHeader::alloc(new_chunk, layout) };
    let Ok(ptr) = res else {
        // Safety: chunk size must fit requested allocation.
        unsafe { unreachable_unchecked() }
    };

    root.set(Some(new_chunk));
    Ok(ptr)
}

#[inline]
unsafe fn resize_fast<T>(
    root: Option<NonNull<ChunkHeader<T>>>,
    ptr: NonNull<u8>,
    old_size: usize,
    new_layout: Layout,
) -> Result<NonNull<[u8]>, Option<usize>>
where
    T: CasPtr,
{
    match root {
        Some(root) => {
            // Safety: `chunk` is a valid pointer to chunk allocation.
            unsafe { ChunkHeader::resize(root, ptr, old_size, new_layout) }
        }
        None => {
            // Allocated memory block exists, so chunks must exist too.
            unreachable_unchecked();
        }
    }
}

#[cold]
unsafe fn resize_slow<T, A>(
    root: impl Mut<Option<NonNull<ChunkHeader<T>>>>,
    chunk_size: usize,
    ptr: NonNull<u8>,
    old_size: usize,
    new_layout: Layout,
    allocator: &A,
) -> Result<NonNull<[u8]>, AllocError>
where
    T: CasPtr,
    A: Allocator,
{
    let new_ptr = alloc_slow(root, chunk_size, new_layout, allocator)?;
    core::ptr::copy_nonoverlapping(
        ptr.as_ptr(),
        new_ptr.as_ptr().cast(),
        new_layout.size().min(old_size),
    );
    // Deallocation is impossible.
    Ok(new_ptr)
}

#[inline]
unsafe fn dealloc<T>(root: Option<NonNull<ChunkHeader<T>>>, ptr: NonNull<u8>, size: usize)
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
#[inline]
pub unsafe fn reset<T, A>(
    root: &mut Option<NonNull<ChunkHeader<T>>>,
    keep_last: bool,
    allocator: &A,
) where
    T: CasPtr,
    A: Allocator,
{
    let mut prev = if keep_last {
        let Some(chunk) = root else {
            return;
        };

        // Safety: `chunk` is a valid pointer to chunk allocation.
        // This function owns mutable reference to `self`.
        unsafe { ChunkHeader::reset(*chunk) }
    } else {
        root.take()
    };

    while let Some(chunk) = prev {
        // Safety: `chunk` is a valid pointer to chunk allocation.
        // Allocated from this allocator with this layout.
        prev = unsafe { ChunkHeader::dealloc_chunk(chunk, allocator) };
    }
}

#[inline]
pub fn reset_leak<T>(root: &mut Option<NonNull<ChunkHeader<T>>>, keep_last: bool)
where
    T: CasPtr,
{
    if keep_last {
        let Some(chunk) = root else {
            return;
        };

        // Safety: `chunk` is a valid pointer to chunk allocation.
        // This function owns mutable reference to `self`.
        unsafe {
            ChunkHeader::reset(*chunk);
        }
    } else {
        *root = None;
    };
}

pub trait Arena {
    /// Allocates memory with `layout` from this arena.
    /// Uses `allocator` to allocate new chunks.
    unsafe fn alloc(
        &self,
        layout: Layout,
        allocator: &impl Allocator,
    ) -> Result<NonNull<[u8]>, AllocError>;

    /// Attempts to resize memory block previously allocated with `Arena`.
    /// If possible shrinks or grows in place.
    unsafe fn resize(
        &self,
        ptr: NonNull<u8>,
        old_size: usize,
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

    /// Reset internals by leaking all chunks.
    /// Useful for cases where leaked memory will be reclaimed
    /// by the allocator.
    /// If `keep_last` is `true`, the last chunk will be kept and reused.
    fn reset_leak(&mut self, keep_last: bool);
}

/// Thread-local arena allocator.
pub struct ArenaLocal {
    root: Cell<Option<NonNull<ChunkHeader<Cell<*mut u8>>>>>,
    min_chunk_size: Cell<usize>,
}

impl Drop for ArenaLocal {
    #[inline]
    fn drop(&mut self) {
        debug_assert!(
            self.root.get().is_none(),
            "Owner must reset `ArenaLocal` with `keep_last` set to `false` before drop"
        );
    }
}

impl ArenaLocal {
    #[inline]
    pub const fn new(min_chunk_size: usize) -> Self {
        ArenaLocal {
            root: Cell::new(None),
            min_chunk_size: Cell::new(min_chunk_size),
        }
    }

    #[inline]
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
    #[inline]
    unsafe fn alloc(
        &self,
        layout: Layout,
        allocator: &impl Allocator,
    ) -> Result<NonNull<[u8]>, AllocError> {
        match alloc_fast(self.root.get(), layout) {
            Ok(ptr) => Ok(ptr),
            Err(None) => Err(AllocError),
            Err(Some(chunk_size)) => alloc_slow(
                &self.root,
                chunk_size.max(self.min_chunk_size.get()),
                layout,
                allocator,
            ),
        }
    }

    #[inline]
    unsafe fn resize(
        &self,
        ptr: NonNull<u8>,
        old_size: usize,
        new_layout: Layout,
        allocator: &impl Allocator,
    ) -> Result<NonNull<[u8]>, AllocError> {
        match resize_fast(self.root.get(), ptr, old_size, new_layout) {
            Ok(ptr) => Ok(ptr),
            Err(None) => Err(AllocError),
            Err(Some(chunk_size)) => resize_slow(
                &self.root,
                chunk_size.max(self.min_chunk_size.get()),
                ptr,
                old_size,
                new_layout,
                allocator,
            ),
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: NonNull<u8>, size: usize) {
        dealloc(self.root.get(), ptr, size)
    }

    #[inline]
    unsafe fn reset(&mut self, keep_last: bool, allocator: &impl Allocator) {
        unsafe { reset(self.root.get_mut(), keep_last, allocator) }
    }

    #[inline]
    fn reset_leak(&mut self, keep_last: bool) {
        reset_leak(self.root.get_mut(), keep_last)
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
        #[inline]
        fn drop(&mut self) {
            debug_assert!(
                self.inner.get_mut().root.is_none(),
                "Owner must reset `ArenaSync` with `keep_last` set to `false` before drop"
            );
        }
    }

    impl ArenaSync {
        #[inline]
        pub const fn new(min_chunk_size: usize) -> Self {
            ArenaSync {
                inner: RwLock::new(Inner {
                    root: None,
                    min_chunk_size,
                }),
            }
        }
    }

    impl Arena for ArenaSync {
        #[inline]
        unsafe fn alloc(
            &self,
            layout: Layout,
            allocator: &impl Allocator,
        ) -> Result<NonNull<[u8]>, AllocError> {
            let inner = self.inner.read();

            match alloc_fast(inner.root, layout) {
                Ok(ptr) => Ok(ptr),
                Err(None) => Err(AllocError),
                Err(Some(chunk_size)) => {
                    drop(inner);
                    let mut guard = self.inner.write();
                    let inner = &mut *guard;

                    alloc_slow(
                        &mut inner.root,
                        chunk_size.max(inner.min_chunk_size),
                        layout,
                        allocator,
                    )
                }
            }
        }

        #[inline]
        unsafe fn resize(
            &self,
            ptr: NonNull<u8>,
            old_size: usize,
            new_layout: Layout,
            allocator: &impl Allocator,
        ) -> Result<NonNull<[u8]>, AllocError> {
            let inner = self.inner.read();
            match resize_fast(inner.root, ptr, old_size, new_layout) {
                Ok(ptr) => Ok(ptr),
                Err(None) => Err(AllocError),
                Err(Some(chunk_size)) => {
                    drop(inner);
                    let mut guard = self.inner.write();
                    let inner = &mut *guard;

                    resize_slow(
                        &mut inner.root,
                        chunk_size.max(inner.min_chunk_size),
                        ptr,
                        old_size,
                        new_layout,
                        allocator,
                    )
                }
            }
        }

        #[inline]
        unsafe fn dealloc(&self, ptr: NonNull<u8>, size: usize) {
            dealloc(self.inner.read().root, ptr, size)
        }

        #[inline]
        unsafe fn reset(&mut self, keep_last: bool, allocator: &impl Allocator) {
            unsafe { reset(&mut self.inner.get_mut().root, keep_last, allocator) }
        }

        #[inline]
        fn reset_leak(&mut self, keep_last: bool) {
            reset_leak(&mut self.inner.get_mut().root, keep_last)
        }
    }
}

#[cfg(feature = "sync")]
pub use self::sync::ArenaSync;

#[cfg(debug_assertions)]
#[track_caller]
unsafe fn unreachable_unchecked() -> ! {
    unreachable!()
}

#[cfg(not(debug_assertions))]
unsafe fn unreachable_unchecked() -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}

#[inline]
unsafe fn memmove(src: *mut u8, dst: *mut u8, size: usize) {
    if src == dst {
        return;
    }

    #[cold]
    #[inline]
    unsafe fn cold_copy(src: *mut u8, dst: *mut u8, size: usize) {
        core::ptr::copy(src, dst, size);
    }

    cold_copy(src, dst, size)
}
