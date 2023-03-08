# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.5] - 2023-03-08

### Fixes
- Allocated memory length calculation.

## [0.2.4] - 2023-03-07

### Fixed

- build warnings


## [0.2.3] - 2023-03-07

### Added 
- `GlobalBlinkAlloc` - allocator suitable for use as `#[global_allocator]`
  Based on `SyncBlinkAlloc`. Provides local proxies that may be `'static`
  Unsafe `reset` method requires that all local proxies
  and previous allocations are dropped.
- `UnsafeGlobalAlloc` - more unsafe version of `GlobalBlinkAlloc`.
  It must never be used concurrently. Only really usable in single-threaded
  applications.
  Requires `unsafe` block to initialize.

## [0.1.0] - 2023-02-27

Initial implementation of blink-allocators.
`BlinkAlloc` for thread-local usage.
`SyncBlinkAlloc` for multi-threaded usage.
`LocalBlinkAlloc` thread-local proxy for `SyncBlinkAlloc`.
`Blink` - friendly allocator adaptor for use without collections.
`BlinkAllocCache` - a cache of `BlinkAlloc` instances to keep them warm
and use from multiple threads.
