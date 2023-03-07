//! This module provide types suitable for use as `#[global_allocator]`.
//!

pub mod local;
#[cfg(feature = "sync")]
pub mod sync;
