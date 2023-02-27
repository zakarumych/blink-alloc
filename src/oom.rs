use core::convert::Infallible;

#[cfg_attr(feature = "alloc", inline(always))]
#[cfg_attr(not(feature = "alloc"), inline(never))]
#[cold]
pub(crate) fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    #[cfg(feature = "alloc")]
    alloc::alloc::handle_alloc_error(layout);

    #[cfg(not(feature = "alloc"))]
    panic!("allocation of {:?} failed", layout);
}

#[inline(never)]
#[cold]
pub(crate) fn size_overflow() -> Infallible {
    panic!("Size overflow")
}
