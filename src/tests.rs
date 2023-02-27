#![cfg(feature = "alloc")]

use core::{
    alloc::Layout,
    cell::Cell,
    mem::{size_of, size_of_val},
    ptr::NonNull,
};

use crate::{blink::Blink, local::BlinkAlloc};

#[test]
fn test_local_alloc() {
    let mut blink = BlinkAlloc::new();

    let ptr = blink
        .allocate(Layout::new::<usize>())
        .unwrap()
        .cast::<usize>();
    unsafe {
        core::ptr::write(ptr.as_ptr(), 42);
    }

    blink.reset();
}

#[test]
fn test_bad_iter() {
    use allocator_api2::{AllocError, Allocator};

    struct OneTimeGlobal {
        served: Cell<bool>,
    }

    unsafe impl Allocator for OneTimeGlobal {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            if self.served.get() {
                Err(allocator_api2::AllocError)
            } else {
                self.served.set(true);
                let ptr = unsafe { alloc::alloc::alloc(layout) };
                let slice = core::ptr::slice_from_raw_parts_mut(ptr, layout.size());
                NonNull::new(slice).ok_or(AllocError)
            }
        }

        unsafe fn deallocate(&self, ptr: core::ptr::NonNull<u8>, layout: Layout) {
            alloc::alloc::dealloc(ptr.as_ptr(), layout)
        }
    }

    const ELEMENT_COUNT: usize = 2000;
    const ELEMENT_SIZE: usize = size_of::<u32>();

    let mut blink = Blink::new_in(BlinkAlloc::with_chunk_size_in(
        ELEMENT_SIZE * ELEMENT_COUNT,
        OneTimeGlobal {
            served: Cell::new(false),
        },
    ));

    blink
        .emplace()
        .from_iter((0..ELEMENT_COUNT as u32).filter(|_| true));

    blink.reset();
}
