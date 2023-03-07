#![cfg(feature = "alloc")]

use core::{alloc::Layout, cell::Cell, mem::size_of, ptr::NonNull};

use allocator_api2::{
    alloc::{AllocError, Allocator, Global},
    vec::Vec,
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
    struct OneTimeGlobal {
        served: Cell<bool>,
    }

    unsafe impl Allocator for OneTimeGlobal {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            if self.served.get() {
                Err(allocator_api2::alloc::AllocError)
            } else {
                self.served.set(true);
                Global.allocate(layout)
            }
        }

        unsafe fn deallocate(&self, ptr: core::ptr::NonNull<u8>, layout: Layout) {
            Global.deallocate(ptr, layout)
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

#[test]
fn test_reuse() {
    struct ControlledGlobal {
        enabled: Cell<bool>,
        last: Cell<bool>,
    }

    unsafe impl Allocator for ControlledGlobal {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            if !self.enabled.get() {
                return Err(AllocError);
            }
            if self.last.get() {
                self.enabled.set(false);
            }
            Global.allocate(layout)
        }

        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
            Global.deallocate(ptr, layout)
        }
    }

    let allocator = ControlledGlobal {
        enabled: Cell::new(true),
        last: Cell::new(false),
    };

    let mut alloc = BlinkAlloc::with_chunk_size_in(0, &allocator);

    for _ in 0..123 {
        alloc.allocate(Layout::new::<u32>()).unwrap();
    }
    alloc.reset();

    allocator.last.set(false);

    for _ in 0..123 {
        alloc.allocate(Layout::new::<u32>()).unwrap();
    }
}

#[test]
fn test_emplace_no_drop() {
    use alloc::{borrow::ToOwned, string::String};

    struct Foo<'a>(&'a String);

    impl Drop for Foo<'_> {
        fn drop(&mut self) {
            panic!("Dropped");
        }
    }

    let mut blink = Blink::new();
    let s = "Hello".to_owned();
    let foo = blink.emplace_no_drop().value(Foo(&s));
    assert_eq!(foo.0, "Hello");
    let world = blink.put("World".to_owned());
    // Would be unsound if `foo` could be dropped.
    foo.0 = world;
    blink.reset();
    // assert_eq!(foo.0, "Universe"); // Cannot compile. `foo` does not outlive reset.
}

#[test]
fn test_vec() {
    let mut blink_alloc = BlinkAlloc::new();
    let mut vec = Vec::new_in(&blink_alloc);
    vec.extend([1, 2, 3]);

    vec.push(4);
    vec.extend(5..6);
    vec.push(6);

    assert_eq!(vec, [1, 2, 3, 4, 5, 6]);
    drop(vec);
    blink_alloc.reset();
}
