#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(feature = "nightly", feature(allocator_api))]

#[cfg(feature = "alloc")]
extern crate alloc;

macro_rules! feature_switch {
    ( ($feature:literal => $with:path | $without:path) ($($args:tt)*)) => {
        #[cfg(feature = $feature)]
        $with!($($args)*);

        #[cfg(not(feature = $feature))]
        $without!($($args)*);
    };
}

#[allow(unused)]
macro_rules! with_default {
    ($(#[$meta:meta])* $v:vis struct $name:ident<$($lt:lifetime,)* $($generic:ident $(: $bound:path $(: $bounds:path )*)? $(= +$default:ty)? $(= $default_type:ty)?),+> { $($(#[$fmeta:meta])*  $fvis:vis $fname:ident: $ftype:ty),* $(,)? }) => {
        $(#[$meta])*
        $v struct $name<$($lt,)* $($generic $(: $bound $(+ $bounds)*)? $(= $default)? $(= $default_type)?)+> {
            $($(#[$fmeta])* $fvis $fname: $ftype,)*
        }
    };
}

#[allow(unused)]
macro_rules! without_default {
    ($(#[$meta:meta])* $v:vis struct $name:ident<$($lt:lifetime,)* $($generic:ident $(: $bound:path $(: $bounds:path )*)? $(= +$default:ty)? $(= $default_type:ty)?),+> { $($(#[$fmeta:meta])* $fvis:vis $fname:ident: $ftype:ty),* $(,)? }) => {
        $(#[$meta])*
        $v struct $name<$($lt,)* $($generic $(: $bound $(+ $bounds)*)? $(= $default_type)?)+> {
            $($(#[$fmeta])* $fvis $fname: $ftype,)*
        }
    };
}

macro_rules! switch_alloc_default {
    ($($args:tt)*) => {
        feature_switch!{("alloc" => with_default | without_default) ($($args)*)}
    };
}

macro_rules! switch_std_default {
    ($($args:tt)*) => {
        feature_switch!{("std" => with_default | without_default) ($($args)*)}
    };
}

mod api;
mod arena;
mod blink;
mod drop_list;
mod global;
mod local;

#[cfg(feature = "sync")]
mod sync;

#[cfg(all(feature = "sync", feature = "alloc"))]
mod cache;

#[cfg(test)]
mod tests;

#[cfg(not(no_global_oom_handling))]
mod oom;

pub use self::{
    api::BlinkAllocator,
    blink::{Blink, Emplace, IteratorExt, SendBlink},
    global::local::UnsafeGlobalBlinkAlloc,
    local::BlinkAlloc,
};

#[cfg(feature = "sync")]
pub use self::sync::{LocalBlinkAlloc, SyncBlinkAlloc};

#[cfg(feature = "sync")]
pub use self::global::sync::GlobalBlinkAlloc;

#[cfg(all(feature = "sync", feature = "alloc"))]
pub use self::cache::BlinkAllocCache;

pub(crate) trait ResultExt<T> {
    fn safe_ok(self) -> T;
}

impl<T> ResultExt<T> for Result<T, core::convert::Infallible> {
    #[inline]
    fn safe_ok(self) -> T {
        match self {
            Ok(value) => value,
            Err(never) => match never {},
        }
    }
}

#[inline]
unsafe fn in_place<'a, T, I>(ptr: *mut T, init: I, f: impl FnOnce(I) -> T) -> &'a mut T {
    // Ask compiler very nicely to store return directly into memory.
    core::ptr::write(ptr, f(init));
    &mut *ptr
}

#[cold]
fn cold() {}

// #[cfg(debug_assertions)]
// #[track_caller]
// unsafe fn unreachable_unchecked() -> ! {
//     unreachable!()
// }

// #[cfg(not(debug_assertions))]
// unsafe fn unreachable_unchecked() -> ! {
//     unsafe { core::hint::unreachable_unchecked() }
// }
