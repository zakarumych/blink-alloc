//! This crate provides `DropList` type which is
//! an intrusive linked list of drop functions.
//!

use core::{
    cell::Cell,
    mem::MaybeUninit,
    ptr::{self, addr_of_mut, slice_from_raw_parts_mut, NonNull},
};

/// Single drop item.
/// Drops associated value when invoked.
struct Drops {
    /// Number of values pointed by `ptr`.
    count: usize,

    /// Drop function.
    drop: unsafe fn(NonNull<Drops>, usize),

    /// Next item in the list.
    next: Option<NonNull<Self>>,
}

impl Drops {
    unsafe fn drop(&self) -> Option<NonNull<Self>> {
        // Safety: `DropItem` constructed as part of `DropItemValue<T>`.
        // And `drop` is set to `drop_in_place::<T>`.
        unsafe { (self.drop)(NonNull::from(&*self), self.count) };
        self.next
    }
}

#[repr(C)]
pub struct DropItem<T: ?Sized> {
    drops: Drops,
    pub value: T,
}

impl<T> DropItem<T> {
    pub unsafe fn init_value<'a, I>(
        mut ptr: NonNull<DropItem<T>>,
        init: I,
        f: impl FnOnce(&mut MaybeUninit<T>, I),
    ) -> &'a mut Self {
        let drops_ptr = addr_of_mut!((*ptr.as_ptr()).drops);
        f(&mut *addr_of_mut!((*ptr.as_ptr()).value).cast(), init);
        ptr::write(
            drops_ptr,
            Drops {
                count: 1,
                drop: drop_from_item::<T>,
                next: None,
            },
        );
        ptr.as_mut()
    }
}

impl<T> DropItem<[T; 0]> {
    pub unsafe fn init_slice<'a>(
        mut ptr: NonNull<DropItem<[T; 0]>>,
        count: usize,
    ) -> (&'a mut Self, &'a mut [T]) {
        debug_assert_ne!(
            count, 0,
            "DropItem<[T]> should not be constructed with count 0"
        );
        ptr::write(
            ptr.as_ptr().cast(),
            Drops {
                count,
                drop: drop_from_item::<T>,
                next: None,
            },
        );
        let slice = core::slice::from_raw_parts_mut(ptr.as_ptr().add(1).cast(), count);
        (ptr.as_mut(), slice)
    }
}

/// Intrusive linked list of drop functions.
pub struct DropList {
    // Root item of the list.
    // Contains `None` if list is empty.
    // Lifetime of the items is bound to `DropList::reset` method calls.
    root: Cell<Option<NonNull<Drops>>>,
}

impl DropList {
    pub const fn new() -> Self {
        DropList {
            root: Cell::new(None),
        }
    }

    /// Adds new drop item for given typed pointer.
    ///
    /// # Safety
    ///
    /// `item` reference must be valid until next call to [`DropList::reset`].
    pub unsafe fn add<'a, T: ?Sized>(&self, item: &'a mut DropItem<T>) -> &'a mut T {
        item.drops.next = self.root.take();
        self.root.set(Some(NonNull::from(&mut item.drops)));
        &mut item.value
    }

    /// Drops all items in the list.
    pub fn reset(&mut self) {
        let mut next = self.root.take();

        while let Some(item_ptr) = next {
            let item = unsafe { item_ptr.as_ref() };

            // Safety: `item` is a valid pointer to `DropItem`.
            // And it didn't move since it was added to the list.
            unsafe {
                next = item.drop();
            }
        }
    }
}

/// Type-erased `core::ptr::drop_in_place` wrapper.
unsafe fn drop_from_item<T>(ptr: NonNull<Drops>, count: usize) {
    let ptr = ptr.cast::<DropItem<T>>();
    let value_ptr = addr_of_mut!((*ptr.as_ptr()).value);
    core::ptr::drop_in_place(slice_from_raw_parts_mut(value_ptr, count))
}
