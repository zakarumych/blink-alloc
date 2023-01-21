//! This module provides `BlinkAlloc` type which is a single-threaded
//! blink allocator.
//!
//!

use core::{
    alloc::Layout,
    convert::identity,
    marker::PhantomData,
    mem::needs_drop,
    ptr::{self, addr_of_mut, copy_nonoverlapping, slice_from_raw_parts_mut, NonNull},
};

use crate::{
    api::{BlinkAllocator, Global},
    drop_list::{DropItem, DropList},
    st::BlinkAlloc,
    BlinkAllocError, ResultExt,
};

/// Safe adaptor for blink allocator.
/// Provides user-friendly safe API for blink allocator usage.
pub struct Blink<A = BlinkAlloc<Global>> {
    drop_list: DropList,
    alloc: A,
}

impl<A> Blink<A>
where
    A: Default,
{
    /// Creates new blink instance with default allocator instance.
    pub fn new() -> Self {
        Self::new_in(Default::default())
    }
}

impl<A> Blink<A> {
    /// Creates new blink instance with provided allocator instance.
    pub fn new_in(alloc: A) -> Self {
        Self {
            drop_list: DropList::new(),
            alloc,
        }
    }
}

impl<A> Blink<A>
where
    A: BlinkAllocator,
{
    /// Returns reference to allocator instance.
    pub fn inner(&self) -> &A {
        &self.alloc
    }

    /// Resets this instance. Drops all allocated values.
    /// And resets associated allocator instance.
    pub fn reset(&mut self) {
        self.drop_list.reset();
        unsafe { self.alloc.reset() };
    }

    unsafe fn _try_emplace_drop<T, I, F>(&self, init: I, f: F) -> Result<&mut T, BlinkAllocError<I>>
    where
        F: FnOnce(I) -> T,
    {
        let item_layout = Layout::new::<DropItem<[T; 0]>>();
        let value_layout = Layout::new::<T>();

        let Ok((full_layout, value_offset)) = item_layout.extend(value_layout) else {
            return Err(BlinkAllocError {
                value: init,
                layout: None,
            });
        };

        let Ok(ptr) = self.alloc.allocate(full_layout) else {
            return Err(BlinkAllocError {
                value: init,
                layout: Some(full_layout),
            });
        };

        let item_ptr = ptr.cast::<DropItem<T>>().as_ptr();
        let value_ptr = unsafe { ptr.as_ptr().add(value_offset).cast::<T>() };

        unsafe {
            ptr::write(value_ptr, f(init));
        }

        // Safety: `item_ptr` is a valid pointer to allocated memory for type `DropItem<T>`.
        let item = unsafe { DropItem::init_value(item_ptr) };
        let reference = self.drop_list.add(item);
        Ok(reference)
    }

    unsafe fn _try_emplace_no_drop<T, I, F>(
        &self,
        init: I,
        f: F,
    ) -> Result<&mut T, BlinkAllocError<I>>
    where
        F: FnOnce(I) -> T,
    {
        let layout = Layout::new::<T>();
        let Ok(ptr) = self.alloc.allocate(layout) else {
            return Err(BlinkAllocError {
                value: init,
                layout: Some(layout),
            });
        };

        let ptr = ptr.cast::<T>().as_ptr();

        // Safety: `ptr` is a valid pointer to allocated memory.
        // Allocated from this allocator with this layout.
        // Duration of the allocation is until next call to [`BlinkAlloc::reset`].
        unsafe {
            ptr::write(ptr, f(init));
        }
        Ok(unsafe { &mut *ptr })
    }

    /// Allocates memory for a value and emplaces value into the memory
    /// using init value and provided closure.
    /// If allocation fails, returns `Err(init)`.
    /// Otherwise calls closure consuming `init`
    /// and initializes memory with closure result.
    #[inline(always)]
    unsafe fn _try_emplace<T, I, F>(
        &self,
        init: I,
        f: F,
        no_drop: bool,
    ) -> Result<&mut T, BlinkAllocError<I>>
    where
        F: FnOnce(I) -> T,
    {
        if !needs_drop::<T>() || no_drop {
            self._try_emplace_no_drop(init, f)
        } else {
            self._try_emplace_drop(init, f)
        }
    }

    unsafe fn _try_emplace_drop_from_iter<T, I>(
        &self,
        mut iter: I,
    ) -> Result<&mut [T], BlinkAllocError<(&mut [T], T)>>
    where
        I: Iterator<Item = T>,
    {
        struct Guard<'a, T: 'a> {
            prev_item: Option<NonNull<DropItem<[T]>>>,
            drop_list: &'a DropList,
        }

        impl<'a, T> Drop for Guard<'a, T> {
            #[inline(always)]
            fn drop(&mut self) {
                self._flush();
            }
        }

        impl<'a, T> Guard<'a, T> {
            #[inline(always)]
            fn set(mut self) -> &'a mut [T] {
                self._flush()
            }

            #[inline(always)]
            fn _flush(&mut self) -> &'a mut [T] {
                if let Some(mut item) = self.prev_item.take() {
                    // Safety: `item` was properly initialized on previous iteration.
                    let item = unsafe { item.as_mut() };
                    unsafe { self.drop_list.add(item) }
                } else {
                    &mut []
                }
            }
        }

        let mut guard = Guard {
            prev_item: None,
            drop_list: &self.drop_list,
        };

        loop {
            let Some(one_more_elem) = iter.next() else {
                return Ok(guard.set());
            };

            let prev_count = match guard.prev_item.as_mut() {
                None => 0,
                Some(item) => {
                    // Safety: `item` was properly initialized on previous iteration.
                    unsafe { item.as_ref().value.len() }
                }
            };

            let (lower, upper) = iter.size_hint();
            let lower_hint = lower.max(prev_count);
            let Some(hint) = upper.unwrap_or(lower_hint).checked_add(1) else {
                    return Err(BlinkAllocError { value: (guard.set(), one_more_elem), layout: None });
            };
            let Some(hint) = hint.checked_add(prev_count) else {
                    return Err(BlinkAllocError { value: (guard.set(), one_more_elem), layout: None });
            };

            let item_layout = Layout::new::<DropItem<[T; 0]>>();
            let Ok(array_layout) = Layout::array::<T>(hint) else {
                    return Err(BlinkAllocError { value: (guard.set(), one_more_elem), layout: None });
            };
            let Ok((full_layout, array_offset)) = item_layout.extend(array_layout) else {
                    return Err(BlinkAllocError { value: (guard.set(), one_more_elem), layout: None });
            };
            let Ok(ptr) = self.alloc.allocate(full_layout) else {
                return Err(BlinkAllocError { value: (guard.set(), one_more_elem), layout: Some(full_layout) });
            };

            let item_ptr = ptr.cast::<DropItem<[T; 0]>>().as_ptr();
            let array_ptr = unsafe { ptr.as_ptr().add(array_offset).cast::<T>() };

            // Takes previous item from guard and copy values to new item.
            if let Some(item) = guard.prev_item.take() {
                // Safety: `item` was properly initialized on previous iteration.
                let slice = unsafe { addr_of_mut!((*item.as_ptr()).value) };

                // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
                // And `hint` is larger than `prev_count` that is size of the `slice`.
                unsafe {
                    copy_nonoverlapping(slice as *const T, array_ptr, prev_count);
                }
            }

            // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
            // And `hint` is larger than `prev_count`
            unsafe {
                ptr::write(array_ptr.add(prev_count), one_more_elem);
            }

            let count =
                iter.by_ref()
                    .take(hint - prev_count - 1)
                    .fold(prev_count + 1, |idx, item| {
                        // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
                        unsafe { ptr::write(array_ptr.add(idx), item) };
                        idx + 1
                    });

            // Safety: `item_ptr` is a valid pointer to allocated memory for type `DropItem<[T; hint]>`.
            // And `count` is not larger than `hint`.
            // And exactly `count` values were initialized.
            let item = unsafe { DropItem::init_slice(item_ptr, count) };
            guard.prev_item = Some(NonNull::from(item));
        }
    }

    unsafe fn _try_emplace_no_drop_from_iter<T, I>(
        &self,
        mut iter: I,
    ) -> Result<&mut [T], BlinkAllocError<(&mut [T], T)>>
    where
        I: Iterator<Item = T>,
    {
        struct State<T> {
            prev_slice: Option<NonNull<[T]>>,
        }

        impl<T> State<T> {
            #[inline(always)]
            fn set<'a>(mut self) -> &'a mut [T] {
                match self.prev_slice.as_mut() {
                    Some(slice) => {
                        // Safety: `slice` was properly initialized on previous iteration.
                        unsafe { slice.as_mut() }
                    }
                    None => &mut [],
                }
            }
        }

        let mut state = State { prev_slice: None };

        loop {
            let Some(one_more_elem) = iter.next() else {
                    return Ok(state.set());
                };

            let prev_count = match state.prev_slice.as_mut() {
                None => 0,
                Some(slice) => {
                    // Safety: `slice` was properly initialized on previous iteration.
                    slice.len()
                }
            };

            let (lower, upper) = iter.size_hint();
            let lower_hint = lower.max(prev_count);
            let Some(hint) = upper.unwrap_or(lower_hint).checked_add(1) else {
                    return Err(BlinkAllocError { value: (state.set(), one_more_elem), layout: None });
                };

            let Some(hint) = hint.checked_add(prev_count) else {
                    return Err(BlinkAllocError { value: (state.set(), one_more_elem), layout: None });
                };

            let Ok(array_layout) = Layout::array::<T>(hint) else {
                    return Err(BlinkAllocError { value: (state.set(), one_more_elem), layout: None });
                };
            let Ok(ptr) = self.alloc.allocate(array_layout) else {
                    return Err(BlinkAllocError { value: (state.set(), one_more_elem), layout: Some(array_layout) });
                };

            let array_ptr = ptr.cast::<T>().as_ptr();

            if let Some(slice) = state.prev_slice.take() {
                // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
                // And `hint` is larger than `prev_count` that is size of the `slice`.
                unsafe {
                    copy_nonoverlapping(slice.as_ptr() as *const T, array_ptr, prev_count);
                }
            }

            // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
            // And `hint` is larger than `prev_count`
            unsafe {
                ptr::write(array_ptr.add(prev_count), one_more_elem);
            }

            let count =
                iter.by_ref()
                    .take(hint - prev_count - 1)
                    .fold(prev_count + 1, |idx, item| {
                        // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
                        unsafe { ptr::write(array_ptr.add(idx), item) };
                        idx + 1
                    });

            // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
            // And `count` is not larger than `hint`.
            // And exactly `count` values were initialized.
            let slice =
                unsafe { NonNull::new_unchecked(slice_from_raw_parts_mut(array_ptr, count)) };
            state.prev_slice = Some(slice);
        }
    }

    /// Allocates memory for a value and emplaces value into the memory
    /// using init value and provided closure.
    /// If allocation fails, returns `Err(init)`.
    /// Otherwise calls closure consuming `init`
    /// and initializes memory with closure result.
    #[inline(always)]
    unsafe fn _try_emplace_from_iter<T, I>(
        &self,
        iter: I,
        no_drop: bool,
    ) -> Result<&mut [T], BlinkAllocError<(&mut [T], T)>>
    where
        I: IntoIterator<Item = T>,
    {
        if !needs_drop::<T>() || no_drop {
            self._try_emplace_no_drop_from_iter(iter.into_iter())
        } else {
            self._try_emplace_drop_from_iter(iter.into_iter())
        }
    }
}

/// Provides interface for emplacing values.
pub struct Emplace<'a, A, T> {
    blink: &'a Blink<A>,
    no_drop: bool,
    marker: PhantomData<fn(T) -> &'a mut T>,
}

impl<'a, A, T> Emplace<'a, A, T>
where
    A: BlinkAllocator,
{
    /// Allocates memory for a value and moves `value` into the memory.
    /// Returns reference to the emplaced value.
    /// If allocation fails, returns `Err(value)`.
    #[inline(always)]
    pub fn try_value(&self, value: T) -> Result<&'a mut T, T> {
        unsafe { self.blink._try_emplace(value, identity, self.no_drop) }.inner_error()
    }

    /// Allocates memory for a value and moves `value` into the memory.
    /// Returns reference to the emplaced value.
    /// If allocation fails, diverges.
    #[inline(always)]
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    pub fn value(&self, value: T) -> &'a mut T {
        unsafe { self.blink._try_emplace(value, identity, self.no_drop) }.handle_alloc_error()
    }

    /// Allocates memory for a value.
    /// On success invokes closure and initialize the value.
    /// Returns reference to the value.
    /// If allocation fails, returns error with closure.
    #[inline(always)]
    pub fn try_value_with<F>(&self, f: F) -> Result<&'a mut T, F>
    where
        F: FnOnce() -> T,
    {
        unsafe { self.blink._try_emplace(f, call_once, self.no_drop) }.inner_error()
    }

    /// Allocates memory for a value.
    /// On success invokes closure and initialize the value.
    /// Returns reference to the value.
    /// If allocation fails, diverges.
    ///
    /// This version works only for `'static` values.
    #[inline(always)]
    pub fn value_with<F>(&self, f: F) -> &'a mut T
    where
        F: FnOnce() -> T,
    {
        unsafe { self.blink._try_emplace(f, call_once, self.no_drop) }.handle_alloc_error()
    }

    /// Allocates memory for an array and initializes it with
    /// values from iterator.
    /// Uses iterator hints to allocate memory.
    /// If iterator yields more values than allocated array can hold,
    /// new larger array is allocated and values are copied.
    /// Repeats until iterator is exhausted.
    /// Works best on iterators that report accurate upper size hint.
    /// Worst case:
    /// `Iterator::size_hint` returns `(M, None)`
    /// and yields `N` values where `N >> M`.
    /// In that case `N / (M + 1)` allocations are performed
    /// and total of `O(N ^ 2 / (M + 1))` memory is used.
    ///
    /// If allocation fails, returns slice of values emplaced so far.
    /// And one element that was taken from iterator and not emplaced.
    #[inline(always)]
    pub fn try_from_iter<I>(&self, iter: I) -> Result<&'a mut [T], (&'a mut [T], T)>
    where
        I: IntoIterator<Item = T>,
    {
        unsafe { self.blink._try_emplace_from_iter(iter, self.no_drop) }.inner_error()
    }

    /// Allocates memory for an array and initializes it with
    /// values from iterator.
    /// Uses iterator hints to allocate memory.
    /// If iterator yields more values than allocated array can hold,
    /// new larger array is allocated and values are copied.
    /// Repeats until iterator is exhausted.
    /// Works best on iterators that report accurate upper size hint.
    /// Worst case:
    /// `Iterator::size_hint` returns `(M, None)`
    /// and yields `N` values where `N >> M`.
    /// In that case `N / (M + 1)` allocations are performed
    /// and total of `O(N ^ 2 / (M + 1))` memory is used.
    ///
    /// If allocation fails, diverges.
    /// Values already emplaced will be dropped.
    /// One last value that was taken from iterator and not emplaced
    /// is dropped before this method returns.
    #[inline(always)]
    pub fn from_iter<I>(&self, iter: I) -> &'a mut [T]
    where
        I: Iterator<Item = T>,
    {
        unsafe { self.blink._try_emplace_from_iter(iter, self.no_drop) }.handle_alloc_error()
    }
}

impl<A> Blink<A>
where
    A: BlinkAllocator,
{
    /// Returns an `Emplace` adaptor that can emplace values into
    /// the blink allocator.
    ///
    /// This version requires the value type to be `'static`.
    /// To use with non-static types consider using one of the following:
    ///
    /// * [`Blink::emplace_no_drop`]
    ///   Causes emplaced value to not be dropped on reset.
    ///   Avoiding potential unsoundness in `Drop` implementation.
    /// * [`Blink::emplace_unchecked`]
    ///   Unsafe version of `emplace`.
    ///   User must guarantee that the value won't have access to references
    ///   allocated by the blink allocator later.
    #[inline(always)]
    pub fn emplace<T: 'static>(&self) -> Emplace<A, T> {
        Emplace {
            blink: self,
            no_drop: false,
            marker: PhantomData,
        }
    }

    /// Returns an `Emplace` adaptor that can emplace values into
    /// the blink allocator.
    ///
    /// This version causes emplaced value to be not-dropped on reset.
    /// To drop returned value on reset, consider one of the following:
    ///
    /// * [`Blink::emplace`]
    ///   Requires the value type to be `'static`.
    /// * [`Blink::emplace_unchecked`]
    ///   Unsafe version of `emplace`.
    ///   User must guarantee that the value won't have access to references
    ///   allocated by the blink allocator later.
    #[inline(always)]
    pub fn emplace_no_drop<T>(&self) -> Emplace<A, T> {
        Emplace {
            blink: self,
            no_drop: true,
            marker: PhantomData,
        }
    }

    /// Returns an `Emplace` adaptor that can emplace values into
    /// the blink allocator.
    ///
    /// This is unsafe version of [`Blink::emplace`].
    /// User must guarantee that values won't attempt to access
    /// memory allocated by the blink allocator later in their [`Drop::drop`]
    /// For safe code consider using one of the following:
    ///
    /// * [`Blink::emplace`]
    ///   Requires the value type to be `'static`.
    /// * [`Blink::emplace_no_drop`]
    ///   Causes emplaced value to not be dropped on reset.
    ///   Avoiding potential unsoundness in `Drop` implementation.
    #[inline(always)]
    pub unsafe fn emplace_unchecked<T>(&self) -> Emplace<A, T> {
        Emplace {
            blink: self,
            no_drop: false,
            marker: PhantomData,
        }
    }
}

#[inline(always)]
fn call_once<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    f()
}
