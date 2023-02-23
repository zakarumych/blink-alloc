use core::alloc::Layout;

/// Aligns `x` down to the nearest multiple of `align`.
pub(crate) fn align_down(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    value & !mask
}

/// Aligns `x` up to the nearest multiple of `align`.
pub(crate) fn align_up(value: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    Some(value.checked_add(mask)? & !mask)
}

/// Aligns pointer `ptr` down to the nearest multiple of `align`.
pub(crate) fn free_aligned_ptr(
    base: *mut u8,
    unused: usize,
    layout: Layout,
) -> Result<usize, usize> {
    debug_assert!(unused > layout.size());

    let addr = sptr::Strict::addr(base);
    let unaligned = addr + unused - layout.size();
    let aligned = align_down(unaligned, layout.align());
    if aligned < addr {
        Err(addr - aligned)
    } else {
        Ok(aligned - addr)
    }
}
