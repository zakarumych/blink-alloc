#[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
#[cfg_attr(feature = "alloc", inline(always))]
#[cfg_attr(not(feature = "alloc"), inline(never))]
#[cold]
pub(crate) fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    #[cfg(feature = "alloc")]
    alloc::alloc::handle_alloc_error(layout);

    #[cfg(not(feature = "alloc"))]
    panic!("allocation of {:?} failed", layout);
}

#[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
#[inline(never)]
#[cold]
pub(crate) fn size_overflow() -> ! {
    panic!("Size overflow")
}
