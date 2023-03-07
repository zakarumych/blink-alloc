# blink-alloc

[![crates](https://img.shields.io/crates/v/blink-alloc.svg?style=for-the-badge&label=blink-alloc)](https://crates.io/crates/blink-alloc)
[![docs](https://img.shields.io/badge/docs.rs-blink--alloc-66c2a5?style=for-the-badge&labelColor=555555&logoColor=white)](https://docs.rs/blink-alloc)
[![actions](https://img.shields.io/github/actions/workflow/status/zakarumych/blink-alloc/badge.yml?branch=main&style=for-the-badge)](https://github.com/zakarumych/blink-alloc/actions/workflows/badge.yml)
[![MIT/Apache](https://img.shields.io/badge/license-MIT%2FApache-blue.svg?style=for-the-badge)](./COPYING)
![loc](https://img.shields.io/tokei/lines/github/zakarumych/blink-alloc?style=for-the-badge)

Blink-alloc is [extremely fast](./BENCHMARKS.md) allocator based on the common idea of
allocating linearly zipping a cursor through memory chunk and
reset everything at once by setting cursor back to start.

With Rust's borrow checker this idea can be implemented safely,
preventing resets to occur while allocated memory is in use.

### [Jump to examples](#examples)

# Design

The blink-allocator acts as an adaptor to some lower-level allocator.
Grabs chunks of memory from underlying allocator
and serves allocations from them.
When chunk is exhausted blink-allocator requests new larger chunk of memory
from lower-level allocator.

On reset all but last chunks are returned back to underlying allocator.
The goal is to allocate a single chunk large enough to serve all allocations
between resets, so that lower-level allocator is not touched after
initial warm-up phase. Making allocations and resets almost free.

The way blink-allocators are implemented offers cheap allocation shrinks when
new layout fits into currently allocated memory block.
Which is certainly the case for things like [`Vec::shrink_to`] and [`Vec::shrink_to_fit`].
When done on the tip of allocation it also frees the memory for reuse.

Additionally fast allocation grows is possible when done on the tip of allocation.
Which is easy to control when using thread-local version.
This opens a way for blazing fast [`Vec::push`] call even at full [`Vec`] capacity.

The simple implementation uses unsynchronized interior mutability
and thus only works in single-thread scenario.
It can be send to another thread but not shared.

Maintaining an instance per thread is one way to tackle this problem.
Yet not always possible or desireable.

This crate provides additional implementation for multi-threaded
scenario with slight performance penalty. To negate it, a local copy
of single-threaded blink-allocator can be created to take memory from
shared multi-threaded blink-allocator, allowing reusing memory across threads
while keeping blazing fast allocations of single-threaded version.
It works best for fork-join type of parallelism since it requires
a point where the multi-threaded blink-allocator is not shared
to reset it.

For task-based parallelism a cache of blink-allocators
can be constructed.
Task may borrow single-threaded blink-allocator and return it when its done.
This will keep cache full of pre-warmed blink-allocators.

# Single-threaded

[`BlinkAlloc`] is a single-threaded version of the allocator.
It uses unsynchronized interior mutability for fastest performance.
[`BlinkAlloc`] can be sent to other threads but not shared.

For multi-threading an instance of [`BlinkAlloc`] per thread/task
can be created.
Though it may be not possible or practical in some ways, so consider
alternatives below.

# Multi-threaded

[`SyncBlinkAlloc`] works in multithreaded environment and shares memory
from one chunk between threads. Reduces overall memory usage,
skips warm-up phase for new threads and tasks.
[`SyncBlinkAlloc`] can spawn [`LocalBlinkAlloc`] instances
that work similarly to [`BlinkAlloc`].
[`LocalBlinkAlloc`] instances fetch memory chunks from shared [`SyncBlinkAlloc`].
[`SyncBlinkAlloc`] still requires exclusive access to reset.
Works best for fork-join style of parallelism, where reset happens
after joining all threads.

For task/based parallelism this crate provides [`BlinkAllocCache`] type
which is a cache of [`BlinkAlloc`] instances.
Tasks may fetch blink allocator from cache,
use it and then return it back to cache.
Cache keeps [`BlinkAlloc`] instances warmed up.

# Allocator API

Allocators implement [`Allocator`] interface from [`alloc`] crate
or a copy of it when feature `"nightly"` is not enabled.
`"nightly"` requires Rust feature [`allocator_api`]
and works only on nightly.
Once [`Allocator`] trait is stable the feature will do nothing and
removed in next major release.

# Blink without collections

[`BlinkAlloc`] and friends implement [`Allocator`] from [`allocator_api`]
unstable feature. It is only available when `"nightly"` feature is enabled
for this crate. Otherwise [`Allocator`] is not [`core::alloc::Allocator`]
but a duplicate defined in the crate.
With [`allocator_api`] it is possible to use [`BlinkAlloc`] and others
for allocator type in collection types that support one.
Currently [`Vec`], [`VecDeque`], [`BTreeMap`] and [`BTreeSet`] can use
user-provided allocator.
Also [`hashbrown::HashMap`] and [`hashbrown::HashSet`] support it with
`"nightly"` feature.

However on stable it cannot be used right now.

It is still possible to use blink-allocators in safe manner -
meet [`Blink`] allocator adaptor.
Put anything into memory allocated by [`Blink`].
Values, iterators, closures to construct a value,
slices and strings.
It works with everything*.
Uses underlying blink-allocator and returns mutable reference
to values placed into allocated memory.
By default it drops placed values on reset.

\* Ask for API extension if doesn't work for your use case.

# Examples

Usage of [`Blink`] allocator adaptor.
Initialize and start putting values there.

```rust
use blink_alloc::Blink;

#[cfg(feature = "alloc")]
fn main() {
    // `Blink::new` uses `BlinkAlloc<Global>`
    let mut blink = Blink::new();

    // Allocates memory and moves value there.
    // Returns mutable reference to it.
    let x = blink.put(42);
    assert_eq!(*x, 42);
    *x = 11;

    // Copies string slice to the blink-allocated memory.
    let string = blink.copy_str("Hello world");

    // Mutable reference allows string mutation
    string.make_ascii_lowercase();
    assert_eq!(string, "hello world");

    // Consumes iterator and returns all values from it in slice.
    // Works fine on problematic iterators with terrible size hint.
    let slice = blink.emplace().from_iter((0..10).filter(|x| x % 3 != 0));
    assert_eq!(&*slice, &[1, 2, 4, 5, 7, 8]);
    blink.reset();
}
#[cfg(not(feature = "alloc"))] fn main() {}
```

```rust
#![cfg_attr(feature = "nightly", feature(allocator_api))]
use blink_alloc::BlinkAlloc;

#[cfg(feature = "alloc")]
fn main() {
    use allocator_api2::vec::Vec;

    let mut blink = BlinkAlloc::new();
    let mut vec = Vec::new_in(&blink);
    vec.extend((1..10).map(|x| x * 3 - 2));

    drop(vec);
    blink.reset();
}
#[cfg(not(feature = "alloc"))] fn main() {}
```

# No-std

This crate supports `no_std` environment.
`"alloc"` feature is enabled by default and adds
dependency on [`alloc`] crate.

## License

Licensed under either of

* Apache License, Version 2.0, ([license/APACHE](license/APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([license/MIT](license/MIT) or <http://opensource.org/licenses/MIT>)

at your option.

## Contributions

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

[`Vec::shrink_to`]: https://doc.rust-lang.org/alloc/vec/struct.Vec.html#method.shrink_to
[`Vec::shrink_to_fit`]: https://doc.rust-lang.org/alloc/vec/struct.Vec.html#method.shrink_to_fit
[`Vec::push`]: https://doc.rust-lang.org/alloc/vec/struct.Vec.html#method.push
[`Vec`]: https://doc.rust-lang.org/alloc/vec/struct.Vec.html
[`BlinkAlloc`]: https://docs.rs/blink-alloc/latest/blink_alloc/struct.BlinkAlloc.html
[`SyncBlinkAlloc`]: https://docs.rs/blink-alloc/latest/blink_alloc/struct.SyncBlinkAlloc.html
[`LocalBlinkAlloc`]: https://docs.rs/blink-alloc/latest/blink_alloc/struct.LocalBlinkAlloc.html
[`BlinkAllocCache`]: https://docs.rs/blink-alloc/latest/blink_alloc/struct.BlinkAllocCache.html
[`Blink`]: https://docs.rs/blink-alloc/latest/blink_alloc/struct.Blink.html
[`Allocator`]: https://docs.rs/allocator-api2/latest/allocator_api2/
[`allocator_api`]: https://doc.rust-lang.org/beta/unstable-book/library-features/allocator-api.html
[`core::alloc::Allocator`]: https://doc.rust-lang.org/core/alloc/trait.Allocator.html
[`Vec`]: https://doc.rust-lang.org/alloc/vec/struct.Vec.html
[`VecDeque`]: https://doc.rust-lang.org/alloc/collections/vec_deque/struct.VecDeque.html
[`BTreeMap`]: https://doc.rust-lang.org/alloc/collections/btree_map/struct.BTreeMap.html
[`BTreeSet`]: https://doc.rust-lang.org/alloc/collections/btree_set/struct.BTreeSet.html
[`hashbrown::HashMap`]: https://docs.rs/hashbrown/latest/hashbrown/hash_map/struct.HashMap.html
[`hashbrown::HashSet`]: https://docs.rs/hashbrown/latest/hashbrown/hash_set/struct.HashSet.html
[`alloc`]: https://doc.rust-lang.org/alloc/index.html
