# Benchmarks

## Table of Contents

- [Benchmark Results](#benchmark-results)
    - [allocation](#allocation)
    - [warm-up](#warm-up)
    - [from-iter](#from-iter)

## Benchmark Results

### allocation

|                                                | `blink_alloc::BlinkAlloc`          | `bumpalo::Bump`                   |
|:-----------------------------------------------|:-----------------------------------|:--------------------------------- |
| **`alloc 4 bytes x 127`**                      | `168.80 ns` (✅ **1.00x**)          | `673.55 ns` (❌ *3.99x slower*)    |
| **`alloc 4 bytes x 1752`**                     | `2.42 us` (✅ **1.00x**)            | `9.27 us` (❌ *3.84x slower*)      |
| **`alloc 4 bytes x 45213`**                    | `62.43 us` (✅ **1.00x**)           | `239.96 us` (❌ *3.84x slower*)    |
| **`alloc 4 bytes, resize to 8 bytes x 127`**   | `450.95 ns` (✅ **1.00x**)          | `1.34 us` (❌ *2.97x slower*)      |
| **`alloc 4 bytes, resize to 8 bytes x 1752`**  | `6.16 us` (✅ **1.00x**)            | `18.41 us` (❌ *2.99x slower*)     |
| **`alloc 4 bytes, resize to 8 bytes x 45213`** | `160.78 us` (✅ **1.00x**)          | `478.41 us` (❌ *2.98x slower*)    |

### warm-up

|                             | `blink_alloc::BlinkAlloc`          | `bumpalo::Bump`                   |
|:----------------------------|:-----------------------------------|:--------------------------------- |
| **`alloc 4 bytes x 127`**   | `382.27 ns` (✅ **1.00x**)          | `669.68 ns` (❌ *1.75x slower*)    |
| **`alloc 4 bytes x 1752`**  | `5.28 us` (✅ **1.00x**)            | `9.30 us` (❌ *1.76x slower*)      |
| **`alloc 4 bytes x 45213`** | `131.85 us` (✅ **1.00x**)          | `239.95 us` (❌ *1.82x slower*)    |

### from-iter

|                                  | `blink_alloc::BlinkAlloc`          | `bumpalo::Bump`                 |
|:---------------------------------|:-----------------------------------|:------------------------------- |
| **`basic x 127`**                | `7.91 us` (✅ **1.00x**)            | `N/A`                           |
| **`basic x 1752`**               | `1.21 ms` (✅ **1.00x**)            | `N/A`                           |
| **`basic x 45213`**              | `1.53 s` (✅ **1.00x**)             | `N/A`                           |
| **`no-drop x 127`**              | `8.25 us` (✅ **1.00x**)            | `8.00 us` (✅ **1.03x faster**)  |
| **`no-drop x 1752`**             | `1.21 ms` (✅ **1.00x**)            | `1.22 ms` (✅ **1.00x slower**)  |
| **`no-drop x 45213`**            | `1.55 s` (✅ **1.00x**)             | `2.73 s` (❌ *1.77x slower*)     |
| **`bad-filter x 127`**           | `12.74 us` (✅ **1.00x**)           | `N/A`                           |
| **`bad-filter x 1752`**          | `2.12 ms` (✅ **1.00x**)            | `N/A`                           |
| **`bad-filter x 45213`**         | `2.12 s` (✅ **1.00x**)             | `N/A`                           |
| **`bad-filter no-drop x 127`**   | `12.74 us` (✅ **1.00x**)           | `N/A`                           |
| **`bad-filter no-drop x 1752`**  | `2.12 ms` (✅ **1.00x**)            | `N/A`                           |
| **`bad-filter no-drop x 45213`** | `2.12 s` (✅ **1.00x**)             | `N/A`                           |

---
Made with [criterion-table](https://github.com/nu11ptr/criterion-table)

