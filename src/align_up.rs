/// Aligns `x` up to the next multiple of `align`.
pub(crate) fn align_up(value: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    Some((value.checked_add(mask)?) & !mask)
}

pub(crate) fn align_ptr_offset(ptr: *mut u8, align: usize) -> usize {
    let addr = ptr as usize;
    match align_up(addr, align) {
        None => usize::MAX,
        Some(aligned) => aligned,
    }
}
