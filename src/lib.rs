#![doc = include_str!("../README.md")]
#![no_std]
#![cfg_attr(feature = "nightly", feature(allocator_api))]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
macro_rules! with_global_default {
    ($(#[$meta:meta])* $v:vis struct $name:ident<$($lt:lifetime,)* $($generic:ident $(: $bound:path $(: $bounds:path )*)? $(= +$global:ty)? $(= $gtype:ty)?),+> { $($fvis:vis $fname:ident: $ftype:ty),* $(,)? }) => {
        $(#[$meta])*
        $v struct $name<$($lt,)* $($generic $(: $bound $(+ $bounds)*)? $(= $global)? $(= $gtype)?)+> {
            $($fvis $fname: $ftype,)*
        }
    };
}

#[cfg(not(feature = "alloc"))]
macro_rules! with_global_default {
    ($(#[$meta:meta])* $v:vis struct $name:ident<$($lt:lifetime,)* $($generic:ident $(: $bound:path $(: $bounds:path )*)? $(= +$global:ty)? $(= $gtype:ty)?),+> { $($fvis:vis $fname:ident: $ftype:ty),* $(,)? }) => {
        $(#[$meta])*
        $v struct $name<$($lt,)* $($generic $(: $bound $(+ $bounds)*)? $(= $gtype)?)+> {
            $($fvis $fname: $ftype,)*
        }
    };
}

mod api;
mod arena;
mod blink;
mod drop_list;
mod local;

#[cfg(feature = "sync")]
mod sync;

#[cfg(test)]
mod tests;

#[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
mod oom;

pub use self::{api::BlinkAllocator, blink::Blink, local::BlinkAlloc};

#[cfg(feature = "sync")]
pub use self::sync::{LocalBlinkAlloc, SyncBlinkAlloc};

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
