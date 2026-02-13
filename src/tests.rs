#![cfg(feature = "alloc")]

use core::{alloc::Layout, cell::Cell, mem::size_of, ptr::NonNull};

#[cfg(feature = "nightly")]
use alloc::{
    alloc::{AllocError, Allocator, Global},
    vec::Vec,
};
#[cfg(not(feature = "nightly"))]
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
                Err(AllocError)
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

#[test]
fn test_tracking() {
    let item_size = 3;
    let layout = Layout::from_size_align(item_size, 1).unwrap();
    let item_count = 11;
    let data_bytes = item_count * item_size;
    assert_eq!(data_bytes, 33);
    // Start with a chunk too small for all data (32 < 33),
    // forcing overflow into a second chunk.
    let initial_chunk_size = 32;
    let initial_chunk_waste = initial_chunk_size % item_size;
    assert_eq!(initial_chunk_waste, 2);

    let mut blink = BlinkAlloc::with_chunk_size(initial_chunk_size);
    assert_eq!(blink.allocated_bytes(), 0);
    assert_eq!(blink.total_capacity(), 0);

    for _ in 0..item_count {
        blink.allocate(layout).unwrap();
    }
    assert_eq!(
        blink.allocated_bytes(),
        data_bytes + initial_chunk_waste,
        "pre-warmup: over-counts by unused tail of exhausted first chunk"
    );

    blink.reset();
    assert_eq!(blink.allocated_bytes(), 0);
    let warmed_cap = blink.total_capacity();
    assert_eq!(warmed_cap, 96);
    assert!(
        warmed_cap >= data_bytes,
        "retained chunk should fit all data"
    );

    for _ in 0..item_count {
        blink.allocate(layout).unwrap();
    }
    assert_eq!(
        blink.allocated_bytes(),
        data_bytes,
        "post-warmup: exact tracking in single chunk"
    );
    assert_eq!(
        blink.total_capacity(),
        warmed_cap,
        "post-warmup: capacity unchanged"
    );
}

#[cfg(feature = "sync")]
#[test]
fn test_tracking_sync() {
    use crate::SyncBlinkAlloc;

    let item_size = 3;
    let layout = Layout::from_size_align(item_size, 1).unwrap();
    let item_count = 11;
    let data_bytes = item_count * item_size;
    assert_eq!(data_bytes, 33);

    let initial_chunk_size = 32;
    let initial_chunk_waste = initial_chunk_size % item_size;
    assert_eq!(initial_chunk_waste, 2);

    let mut blink = SyncBlinkAlloc::with_chunk_size_in(initial_chunk_size, Global);
    assert_eq!(blink.allocated_bytes(), 0);
    assert_eq!(blink.total_capacity(), 0);

    for _ in 0..item_count {
        blink.allocate(layout).unwrap();
    }
    assert_eq!(
        blink.allocated_bytes(),
        data_bytes + initial_chunk_waste,
        "pre-warmup: over-counts by unused tail of exhausted first chunk"
    );

    blink.reset();
    assert_eq!(blink.allocated_bytes(), 0);
    let warmed_cap = blink.total_capacity();
    assert_eq!(warmed_cap, 96);

    for _ in 0..item_count {
        blink.allocate(layout).unwrap();
    }
    assert_eq!(
        blink.allocated_bytes(),
        data_bytes,
        "post-warmup: exact tracking in single chunk"
    );
    assert_eq!(
        blink.total_capacity(),
        warmed_cap,
        "post-warmup: capacity unchanged"
    );
}

#[cfg(feature = "sync")]
#[test]
fn test_tracking_local_proxy() {
    use crate::SyncBlinkAlloc;

    /// ChunkHeader has 4 usize fields: cursor, end, prev, cumulative_size.
    const CHUNK_HEADER_SIZE: usize = size_of::<usize>() * 4;

    let item_size = 3;
    let layout = Layout::from_size_align(item_size, 1).unwrap();
    let item_count = 11;
    let data_bytes = item_count * item_size;
    assert_eq!(data_bytes, 33);

    let initial_chunk_size = 32;
    let initial_chunk_waste = initial_chunk_size % item_size;
    assert_eq!(initial_chunk_waste, 2);
    let shared_chunk_size = 512;
    let mut shared = SyncBlinkAlloc::with_chunk_size_in(shared_chunk_size, Global);
    assert_eq!(shared.allocated_bytes(), 0);
    assert_eq!(shared.total_capacity(), 0);

    let local = shared.local();
    assert_eq!(local.allocated_bytes(), 0);
    assert_eq!(local.total_capacity(), 0);

    for _ in 0..item_count {
        local.allocate(layout).unwrap();
    }
    assert_eq!(
        local.allocated_bytes(),
        data_bytes + initial_chunk_waste,
        "local over-counts by chunk tail waste, same as BlinkAlloc"
    );

    let local_chunk_1_cap = 32;
    let local_chunk_2_cap = 96;
    assert_eq!(
        local.total_capacity(),
        local_chunk_1_cap + local_chunk_2_cap,
        "local has two chunks allocated from shared"
    );

    let local_chunk_bytes =
        (local_chunk_1_cap + CHUNK_HEADER_SIZE) + (local_chunk_2_cap + CHUNK_HEADER_SIZE);
    assert_eq!(local_chunk_bytes, 192);
    assert_eq!(
        shared.allocated_bytes(),
        local_chunk_bytes,
        "shared sees local's chunk allocations, not individual items"
    );

    let shared_cap =
        (shared_chunk_size + CHUNK_HEADER_SIZE).next_power_of_two() - CHUNK_HEADER_SIZE;
    assert_eq!(shared_cap, 992);
    assert_eq!(shared.total_capacity(), shared_cap);

    drop(local);
    assert_eq!(
        shared.allocated_bytes(),
        local_chunk_bytes,
        "shared still holds local's chunk memory after drop"
    );
    assert_eq!(
        shared.total_capacity(),
        shared_cap,
        "shared capacity does not change from dropping local"
    );

    shared.reset();
    assert_eq!(shared.allocated_bytes(), 0);
    assert_eq!(shared.total_capacity(), shared_cap);
}
