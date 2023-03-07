use blink_alloc::GlobalBlinkAlloc;

#[global_allocator]
static GLOBAL_ALLOC: GlobalBlinkAlloc = GlobalBlinkAlloc::new();

fn main() {
    unsafe {
        GLOBAL_ALLOC.blink_mode();
    }

    let _ = Box::new(42);
    let _ = vec![1, 2, 3];

    unsafe {
        GLOBAL_ALLOC.direct_mode();
    }
}
