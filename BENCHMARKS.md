# Benchmarks

## Table of Contents

- [Benchmark Results](#benchmark-results)
    - [allocation](#allocation)
    - [warm-up](#warm-up)
    - [vec](#vec)
    - [from-iter](#from-iter)

## Benchmark Results

### allocation

|                                    | `blink_alloc::BlinkAlloc`          | `blink_alloc::SyncBlinkAlloc`          | `bumpalo::Bump`                   |
|:-----------------------------------|:-----------------------------------|:---------------------------------------|:--------------------------------- |
| **`alloc x 17453`**                | `23.79 us` (✅ **1.00x**)           | `169.62 us` (❌ *7.13x slower*)         | `91.14 us` (❌ *3.83x slower*)     |
| **`grow same align x 17453`**      | `59.37 us` (✅ **1.00x**)           | `340.33 us` (❌ *5.73x slower*)         | `152.35 us` (❌ *2.57x slower*)    |
| **`grow smaller align x 17453`**   | `59.49 us` (✅ **1.00x**)           | `340.34 us` (❌ *5.72x slower*)         | `151.47 us` (❌ *2.55x slower*)    |
| **`grow larger align x 17453`**    | `99.36 us` (✅ **1.00x**)           | `341.84 us` (❌ *3.44x slower*)         | `183.13 us` (❌ *1.84x slower*)    |
| **`shrink same align x 17453`**    | `53.86 us` (✅ **1.00x**)           | `340.96 us` (❌ *6.33x slower*)         | `103.17 us` (❌ *1.92x slower*)    |
| **`shrink smaller align x 17453`** | `53.82 us` (✅ **1.00x**)           | `341.15 us` (❌ *6.34x slower*)         | `100.82 us` (❌ *1.87x slower*)    |
| **`shrink larger align x 17453`**  | `87.50 us` (✅ **1.00x**)           | `342.38 us` (❌ *3.91x slower*)         | `fails`                             |

### warm-up

|                             | `blink_alloc::BlinkAlloc`          | `blink_alloc::SyncBlinkAlloc`          | `bumpalo::Bump`                  |
|:----------------------------|:-----------------------------------|:---------------------------------------|:-------------------------------- |
| **`alloc 4 bytes x 17453`** | `24.39 us` (✅ **1.00x**)           | `170.02 us` (❌ *6.97x slower*)         | `91.73 us` (❌ *3.76x slower*)    |

### vec

|                                | `blink_alloc::BlinkAlloc`          | `blink_alloc::SyncBlinkAlloc`          | `bumpalo::Bump`                   |
|:-------------------------------|:-----------------------------------|:---------------------------------------|:--------------------------------- |
| **`push x 17453`**             | `36.96 us` (✅ **1.00x**)           | `37.01 us` (✅ **1.00x slower**)        | `42.26 us` (❌ *1.14x slower*)     |
| **`reserve_exact(1) x 17453`** | `63.87 us` (✅ **1.00x**)           | `169.10 us` (❌ *2.65x slower*)         | `8.64 ms` (❌ *135.21x slower*)    |


### from-iter

|                                  | `blink_alloc::BlinkAlloc`          | `blink_alloc::SyncBlinkAlloc`          | `bumpalo::Bump`                 |
|:---------------------------------|:-----------------------------------|:---------------------------------------|:------------------------------- |
| **`basic x 17453`**              | `1.11 ms` (✅ **1.00x**)            | `1.11 ms` (✅ **1.00x faster**)         | `N/A`                           |
| **`no-drop x 17453`**            | `1.10 ms` (✅ **1.00x**)            | `1.12 ms` (✅ **1.02x slower**)         | `1.36 ms` (❌ *1.24x slower*)    |
| **`bad-filter x 17453`**         | `1.67 ms` (✅ **1.00x**)            | `1.77 ms` (✅ **1.06x slower**)         | `N/A`                           |
| **`bad-filter no-drop x 17453`** | `1.67 ms` (✅ **1.00x**)            | `1.77 ms` (✅ **1.06x slower**)         | `N/A`                           |

---
Made with [criterion-table](https://github.com/nu11ptr/criterion-table)

