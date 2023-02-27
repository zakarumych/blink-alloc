#![cfg(feature = "alloc")]

use core::alloc::Layout;

#[test]
fn test_local_alloc() {
    use crate::local::BlinkAlloc;

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
