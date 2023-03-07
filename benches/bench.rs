#![cfg_attr(feature = "nightly", feature(allocator_api))]

use std::{
    alloc::System,
    any::TypeId,
    sync::atomic::{AtomicUsize, Ordering},
};

use allocator_api2::{
    alloc::{Allocator, Layout},
    vec::Vec,
};
use blink_alloc::*;
use criterion::*;

/// GlobalAlloc that counts the number of allocations and deallocations
/// and number of bytes allocated and deallocated.
#[cfg(feature = "bench-with-counting-allocator")]
struct CountingGlobalAlloc {
    allocations: AtomicUsize,
    deallocations: AtomicUsize,

    bytes_allocated: AtomicUsize,
    bytes_deallocated: AtomicUsize,
}

#[cfg(feature = "bench-with-counting-allocator")]
impl CountingGlobalAlloc {
    pub const fn new() -> Self {
        CountingGlobalAlloc {
            allocations: AtomicUsize::new(0),
            deallocations: AtomicUsize::new(0),
            bytes_allocated: AtomicUsize::new(0),
            bytes_deallocated: AtomicUsize::new(0),
        }
    }

    pub fn reset_stat(&self) {
        self.allocations.store(0, Ordering::Relaxed);
        self.deallocations.store(0, Ordering::Relaxed);
        self.bytes_allocated.store(0, Ordering::Relaxed);
        self.bytes_deallocated.store(0, Ordering::Relaxed);
    }

    pub fn print_stat(&self) {
        let allocations = self.allocations.load(Ordering::Relaxed);
        let deallocations = self.deallocations.load(Ordering::Relaxed);
        let bytes_allocated = self.bytes_allocated.load(Ordering::Relaxed);
        let bytes_deallocated = self.bytes_deallocated.load(Ordering::Relaxed);

        eprintln!(
            "allocations: {allocations},
            deallocations: {deallocations},
            bytes_allocated: {bytes_allocated},
            bytes_deallocated: {bytes_deallocated}"
        )
    }
}

#[cfg(feature = "bench-with-counting-allocator")]
unsafe impl core::alloc::GlobalAlloc for CountingGlobalAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            self.allocations.fetch_add(1, Ordering::Relaxed);
            self.bytes_allocated
                .fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        self.deallocations.fetch_add(1, Ordering::Relaxed);
        self.bytes_deallocated
            .fetch_add(layout.size(), Ordering::Relaxed);
    }
}

#[cfg(feature = "bench-with-counting-allocator")]
#[global_allocator]
static COUNTING_ALLOCATOR: CountingGlobalAlloc = CountingGlobalAlloc::new();

#[inline]
fn print_mem_stat() {
    #[cfg(feature = "bench-with-counting-allocator")]
    COUNTING_ALLOCATOR.print_stat();
}

#[inline]
fn reset_mem_stat() {
    #[cfg(feature = "bench-with-counting-allocator")]
    COUNTING_ALLOCATOR.reset_stat();
}

trait BumpAllocator
where
    for<'a> &'a Self: Allocator,
{
    fn reset(&mut self);
}

impl BumpAllocator for BlinkAlloc {
    #[inline(always)]
    fn reset(&mut self) {
        self.reset();
    }
}

impl BumpAllocator for SyncBlinkAlloc {
    #[inline(always)]
    fn reset(&mut self) {
        self.reset();
    }
}

impl BumpAllocator for LocalBlinkAlloc<'_> {
    #[inline(always)]
    fn reset(&mut self) {
        self.reset();
    }
}

impl BumpAllocator for bumpalo::Bump {
    #[inline(always)]
    fn reset(&mut self) {
        self.reset();
    }
}

trait SyncBumpAllocator: BumpAllocator + Sync
where
    for<'a> &'a Self: Allocator,
{
    type Local<'a>: BlinkAllocator
    where
        Self: 'a;

    fn local(&self) -> Self::Local<'_>;
}

trait Adaptor {
    const CAN_DROP: bool;
    const ANY_ITER: bool;

    fn put<T: 'static>(&self, value: T) -> &mut T;
    fn put_no_drop<T>(&self, value: T) -> &mut T;
    fn copy_slice<T: Copy>(&self, slice: &[T]) -> &mut [T];
    fn copy_str(&self, string: &str) -> &mut str;
    fn from_iter<T: 'static>(&self, iter: impl Iterator<Item = T>) -> &mut [T];
    fn from_iter_no_drop<T>(&self, iter: impl Iterator<Item = T>) -> &mut [T];

    #[inline(always)]
    fn from_exact_size_iter_no_drop<T>(&self, iter: impl ExactSizeIterator<Item = T>) -> &mut [T] {
        self.from_iter_no_drop(iter)
    }

    fn reset(&mut self);
}

impl<A> Adaptor for Blink<A>
where
    A: BlinkAllocator,
{
    const CAN_DROP: bool = true;
    const ANY_ITER: bool = true;

    #[inline(always)]
    fn put<T: 'static>(&self, value: T) -> &mut T {
        self.put(value)
    }

    #[inline(always)]
    fn put_no_drop<T>(&self, value: T) -> &mut T {
        self.emplace_no_drop().value(value)
    }

    #[inline(always)]
    fn copy_slice<T: Copy>(&self, slice: &[T]) -> &mut [T] {
        self.copy_slice(slice)
    }

    #[inline(always)]
    fn copy_str(&self, string: &str) -> &mut str {
        self.copy_str(string)
    }

    #[inline(always)]
    fn from_iter<T: 'static>(&self, iter: impl Iterator<Item = T>) -> &mut [T] {
        self.emplace().from_iter(iter)
    }

    #[inline(always)]
    fn from_iter_no_drop<T>(&self, iter: impl Iterator<Item = T>) -> &mut [T] {
        self.emplace_no_drop().from_iter(iter)
    }

    #[inline(always)]
    fn reset(&mut self) {
        self.reset();
    }
}

impl Adaptor for bumpalo::Bump {
    const CAN_DROP: bool = false;
    const ANY_ITER: bool = false;

    #[inline(always)]
    fn put<T: 'static>(&self, _value: T) -> &mut T {
        unimplemented!()
    }

    #[inline(always)]
    fn put_no_drop<T>(&self, value: T) -> &mut T {
        self.alloc(value)
    }

    #[inline(always)]
    fn copy_slice<T: Copy>(&self, slice: &[T]) -> &mut [T] {
        self.alloc_slice_copy(slice)
    }

    #[inline(always)]
    fn copy_str(&self, string: &str) -> &mut str {
        self.alloc_str(string)
    }

    #[inline(always)]
    fn from_iter<T: 'static>(&self, _iter: impl Iterator<Item = T>) -> &mut [T] {
        unimplemented!()
    }

    #[inline(always)]
    fn from_iter_no_drop<T>(&self, _iter: impl Iterator<Item = T>) -> &mut [T] {
        unimplemented!()
    }

    fn from_exact_size_iter_no_drop<T>(&self, iter: impl ExactSizeIterator<Item = T>) -> &mut [T] {
        self.alloc_slice_fill_iter(iter)
    }

    #[inline(always)]
    fn reset(&mut self) {
        self.reset();
    }
}

const SIZE: usize = 17453;

fn bench_alloc<A>(name: &str, c: &mut Criterion)
where
    for<'a> &'a A: Allocator,
    A: BumpAllocator + Default + 'static,
{
    let mut group = c.benchmark_group(format!("allocation/{name}"));

    reset_mem_stat();
    let mut alloc = A::default();

    // Pre-warm the allocator
    (&alloc).allocate(Layout::new::<[u32; 65536]>()).unwrap();
    alloc.reset();

    group.bench_function(format!("alloc x {SIZE}"), |b| {
        b.iter(|| {
            for _ in 0..SIZE {
                black_box((&alloc).allocate(Layout::new::<u32>()).unwrap());
            }
            alloc.reset();
        })
    });

    print_mem_stat();
    reset_mem_stat();
    // let mut alloc = A::default();

    group.bench_function(format!("grow same align x {SIZE}"), |b| {
        b.iter(|| {
            for _ in 0..SIZE {
                unsafe {
                    let ptr = (&alloc).allocate(Layout::new::<u32>()).unwrap();
                    let ptr = (&alloc)
                        .grow(ptr.cast(), Layout::new::<u32>(), Layout::new::<[u32; 2]>())
                        .unwrap();
                    black_box(ptr);
                }
            }
            alloc.reset();
        })
    });

    group.bench_function(format!("grow smaller align x {SIZE}"), |b| {
        b.iter(|| {
            for _ in 0..SIZE {
                unsafe {
                    let ptr = (&alloc).allocate(Layout::new::<u32>()).unwrap();
                    let ptr = (&alloc)
                        .grow(ptr.cast(), Layout::new::<u32>(), Layout::new::<[u16; 4]>())
                        .unwrap();
                    black_box(ptr);
                }
            }
            alloc.reset();
        })
    });

    group.bench_function(format!("grow larger align x {SIZE}"), |b| {
        b.iter(|| {
            for _ in 0..SIZE {
                unsafe {
                    let ptr = (&alloc).allocate(Layout::new::<u32>()).unwrap();
                    let ptr = (&alloc)
                        .grow(ptr.cast(), Layout::new::<u32>(), Layout::new::<u64>())
                        .unwrap();
                    black_box(ptr);
                }
            }
            alloc.reset();
        })
    });

    group.bench_function(format!("shrink same align x {SIZE}"), |b| {
        b.iter(|| {
            for _ in 0..SIZE {
                unsafe {
                    let ptr = (&alloc).allocate(Layout::new::<[u32; 2]>()).unwrap();
                    let ptr = (&alloc)
                        .shrink(ptr.cast(), Layout::new::<[u32; 2]>(), Layout::new::<u32>())
                        .unwrap();
                    black_box(ptr);
                }
            }
            alloc.reset();
        })
    });

    group.bench_function(format!("shrink smaller align x {SIZE}"), |b| {
        b.iter(|| {
            for _ in 0..SIZE {
                unsafe {
                    let ptr = (&alloc).allocate(Layout::new::<u32>()).unwrap();
                    let ptr = (&alloc)
                        .shrink(ptr.cast(), Layout::new::<u32>(), Layout::new::<u16>())
                        .unwrap();
                    black_box(ptr);
                }
            }
            alloc.reset();
        })
    });

    if TypeId::of::<A>() != TypeId::of::<bumpalo::Bump>() {
        group.bench_function(format!("shrink larger align x {SIZE}"), |b| {
            b.iter(|| {
                for _ in 0..SIZE {
                    unsafe {
                        let ptr = (&alloc).allocate(Layout::new::<[u32; 4]>()).unwrap();
                        let ptr = (&alloc)
                            .shrink(ptr.cast(), Layout::new::<[u32; 4]>(), Layout::new::<u64>())
                            .unwrap();
                        black_box(ptr);
                    }
                }
                alloc.reset();
            })
        });
    }

    print_mem_stat();

    group.finish();
}

fn bench_warm_up<A>(name: &str, c: &mut Criterion)
where
    for<'a> &'a A: Allocator,
    A: BumpAllocator + Default,
{
    let mut group = c.benchmark_group(format!("warm-up/{name}"));

    reset_mem_stat();

    group.bench_function(format!("alloc 4 bytes x {SIZE}"), |b| {
        b.iter(|| {
            let alloc = A::default();
            for _ in 0..SIZE {
                black_box((&alloc).allocate(Layout::new::<u32>()).unwrap());
            }
        })
    });

    print_mem_stat();
    group.finish();
}

fn bench_vec<A>(name: &str, c: &mut Criterion)
where
    for<'a> &'a A: Allocator,
    A: BumpAllocator + Default,
{
    let mut group = c.benchmark_group(format!("vec/{name}"));

    reset_mem_stat();
    let mut alloc = A::default();

    // Pre-warm the allocator
    (&alloc).allocate(Layout::new::<[u32; 65536]>()).unwrap();
    alloc.reset();

    group.bench_function(format!("push x {SIZE}"), |b| {
        b.iter(|| {
            let mut vec = Vec::new_in(&alloc);
            for i in 0..SIZE {
                vec.push(i);
            }
            drop(vec);
            alloc.reset();
        })
    });

    group.bench_function(format!("reserve_exact(1) x {SIZE}"), |b| {
        b.iter(|| {
            let mut vec = Vec::<u32, &A>::new_in(&alloc);
            for i in 0..SIZE {
                vec.reserve_exact(i);
            }
            drop(vec);
            alloc.reset();
        })
    });

    print_mem_stat();
    reset_mem_stat();

    group.finish();
}

fn bench_from_iter<A>(name: &str, c: &mut Criterion)
where
    A: Adaptor + Default,
{
    let mut group = c.benchmark_group(format!("from-iter/{name}"));

    reset_mem_stat();
    let mut adaptor = A::default();

    // Pre-warm the allocator
    adaptor.from_exact_size_iter_no_drop((0..65536).map(|_| 0u32));
    adaptor.reset();

    if A::CAN_DROP && A::ANY_ITER {
        group.bench_function(format!("basic x {SIZE}"), |b| {
            b.iter(|| {
                for _ in 0..SIZE {
                    black_box(adaptor.from_iter((0..111).map(|_| black_box(0u32))));
                }
                adaptor.reset();
            })
        });

        print_mem_stat();
        reset_mem_stat();
    }

    group.bench_function(format!("no-drop x {SIZE}"), |b| {
        b.iter(|| {
            for _ in 0..SIZE {
                black_box(adaptor.from_exact_size_iter_no_drop((0..111).map(|_| black_box(0u32))));
            }
            adaptor.reset();
        })
    });

    print_mem_stat();
    reset_mem_stat();

    if A::CAN_DROP && A::ANY_ITER {
        group.bench_function(format!("bad-filter x {SIZE}"), |b| {
            b.iter(|| {
                for _ in 0..SIZE {
                    black_box(
                        adaptor.from_iter(
                            (0..111)
                                .filter(|_| black_box(true))
                                .map(|_| black_box(0u32)),
                        ),
                    );
                }
                adaptor.reset();
            })
        });

        print_mem_stat();
        reset_mem_stat();
    }

    if A::ANY_ITER {
        group.bench_function(format!("bad-filter no-drop x {SIZE}"), |b| {
            b.iter(|| {
                for _ in 0..SIZE {
                    black_box(
                        adaptor.from_iter_no_drop(
                            (0..111)
                                .filter(|_| black_box(true))
                                .map(|_| black_box(0u32)),
                        ),
                    );
                }
                adaptor.reset();
            })
        });

        print_mem_stat();
    }

    group.finish();
}

pub fn criterion_benchmark(c: &mut Criterion) {
    bench_alloc::<BlinkAlloc>("blink_alloc::BlinkAlloc", c);
    bench_alloc::<SyncBlinkAlloc>("blink_alloc::SyncBlinkAlloc", c);
    bench_alloc::<bumpalo::Bump>("bumpalo::Bump", c);

    bench_warm_up::<BlinkAlloc>("blink_alloc::BlinkAlloc", c);
    bench_warm_up::<SyncBlinkAlloc>("blink_alloc::SyncBlinkAlloc", c);
    bench_warm_up::<bumpalo::Bump>("bumpalo::Bump", c);

    bench_vec::<BlinkAlloc>("blink_alloc::BlinkAlloc", c);
    bench_vec::<SyncBlinkAlloc>("blink_alloc::SyncBlinkAlloc", c);
    bench_vec::<bumpalo::Bump>("bumpalo::Bump", c);

    bench_from_iter::<Blink<BlinkAlloc>>("blink_alloc::BlinkAlloc", c);
    bench_from_iter::<Blink<SyncBlinkAlloc>>("blink_alloc::SyncBlinkAlloc", c);
    bench_from_iter::<bumpalo::Bump>("bumpalo::Bump", c);
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
