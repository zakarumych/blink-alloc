# Benchmarks

Measurements done on MacOS with M1 processor.

## Table of Contents

- [Benchmark Results](#benchmark-results)
    - [allocation](#allocation)
    - [warm-up](#warm-up)
    - [from-iter](#from-iter)

## Benchmark Results

### allocation

|                                                | `blink_alloc::BlinkAlloc`          | `bumpalo::Bump`                   |
|:-----------------------------------------------|:-----------------------------------|:--------------------------------- |
| **`alloc 4 bytes x 127`**                      | `414.26 ns` (✅ **1.00x**)          | `663.48 ns` (❌ *1.60x slower*)    |
| **`alloc 4 bytes x 1752`**                     | `5.72 us` (✅ **1.00x**)            | `9.15 us` (❌ *1.60x slower*)      |
| **`alloc 4 bytes x 45213`**                    | `146.51 us` (✅ **1.00x**)          | `236.26 us` (❌ *1.61x slower*)    |
| **`alloc 4 bytes, resize to 8 bytes x 127`**   | `483.48 ns` (✅ **1.00x**)          | `1.33 us` (❌ *2.76x slower*)      |
| **`alloc 4 bytes, resize to 8 bytes x 1752`**  | `6.70 us` (✅ **1.00x**)            | `18.44 us` (❌ *2.75x slower*)     |
| **`alloc 4 bytes, resize to 8 bytes x 45213`** | `174.29 us` (✅ **1.00x**)          | `475.71 us` (❌ *2.73x slower*)    |

### warm-up

|                             | `blink_alloc::BlinkAlloc`          | `bumpalo::Bump`                   |
|:----------------------------|:-----------------------------------|:--------------------------------- |
| **`alloc 4 bytes x 127`**   | `419.80 ns` (✅ **1.00x**)          | `666.72 ns` (❌ *1.59x slower*)    |
| **`alloc 4 bytes x 1752`**  | `5.77 us` (✅ **1.00x**)            | `9.23 us` (❌ *1.60x slower*)      |
| **`alloc 4 bytes x 45213`** | `147.64 us` (✅ **1.00x**)          | `237.36 us` (❌ *1.61x slower*)    |

### from-iter

|                                  | `blink_alloc::BlinkAlloc`          | `bumpalo::Bump`                 |
|:---------------------------------|:-----------------------------------|:------------------------------- |
| **`basic x 127`**                | `7.85 us` (✅ **1.00x**)            | `N/A`                           |
| **`basic x 1752`**               | `1.20 ms` (✅ **1.00x**)            | `N/A`                           |
| **`basic x 45213`**              | `1.59 s` (✅ **1.00x**)             | `N/A`                           |
| **`no-drop x 127`**              | `8.00 us` (✅ **1.00x**)            | `8.06 us` (✅ **1.01x slower**)  |
| **`no-drop x 1752`**             | `1.21 ms` (✅ **1.00x**)            | `1.22 ms` (✅ **1.01x slower**)  |
| **`no-drop x 45213`**            | `1.50 s` (✅ **1.00x**)             | `2.76 s` (❌ *1.83x slower*)     |
| **`bad-filter x 127`**           | `12.75 us` (✅ **1.00x**)           | `N/A`                           |
| **`bad-filter x 1752`**          | `2.12 ms` (✅ **1.00x**)            | `N/A`                           |
| **`bad-filter x 45213`**         | `2.13 s` (✅ **1.00x**)             | `N/A`                           |
| **`bad-filter no-drop x 127`**   | `12.75 us` (✅ **1.00x**)           | `N/A`                           |
| **`bad-filter no-drop x 1752`**  | `2.12 ms` (✅ **1.00x**)            | `N/A`                           |
| **`bad-filter no-drop x 45213`** | `2.14 s` (✅ **1.00x**)             | `N/A`                           |

---
Made with [criterion-table](https://github.com/nu11ptr/criterion-table)

