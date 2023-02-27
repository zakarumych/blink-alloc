//! Provides `Blink` allocator adaptor.

use core::{
    alloc::Layout,
    convert::{identity, Infallible},
    marker::PhantomData,
    mem::{needs_drop, size_of, ManuallyDrop, MaybeUninit},
    ptr::{self, NonNull},
};

#[cfg(feature = "alloc")]
use allocator_api2::Global;

use crate::{
    api::BlinkAllocator,
    drop_list::{DropItem, DropList},
    in_place,
};

#[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
use crate::ResultExt;

#[cfg(feature = "alloc")]
use crate::local::BlinkAlloc;

#[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
use crate::oom::{handle_alloc_error, size_overflow};

type EmplaceType<T, E> = Result<T, ManuallyDrop<E>>;
type EmplaceSlot<T, E> = MaybeUninit<EmplaceType<T, E>>;

pub trait CoerceFromMut<'a, T: ?Sized> {
    fn coerce(t: &'a mut T) -> Self;
}

impl<'a, T: ?Sized> CoerceFromMut<'a, T> for &'a mut T {
    #[inline(always)]
    fn coerce(t: &'a mut T) -> Self {
        t
    }
}

impl<'a, T: ?Sized> CoerceFromMut<'a, T> for &'a T {
    #[inline(always)]
    fn coerce(t: &'a mut T) -> Self {
        t
    }
}

with_global_default! {
    /// An allocator adaptor for designed for blink allocator.
    /// Provides user-friendly methods to emplace values into allocated memory.
    /// Supports emplace existing, constructing value in allocated memory directly or indirectly.
    /// And also emplacing items yield from iterator into contiguous memory and returning slice reference.
    ///
    /// [`Blink`] calls [`Drop::drop`] for emplaced values when reset or dropped.
    /// This allows to use [`Blink`] instead of collections in some scenarios without needing to enable [`allocation_api`] feature.
    ///
    /// A blink-allocator adapter for user-friendly safe allocations
    /// without use of collections.
    ///
    /// Provides an ability to emplace values into allocated memory,
    /// baked by the associated blink-allocator instance.
    /// And returns mutable reference to the value.
    /// Values can be emplaced by move, by construction in allocated memory
    /// (when compiler likes us), from iterators etc.
    /// Most operations are provided in two flavors:
    /// `try_` prefixed methods returns `Result` with allocation errors.
    /// And non-prefixed methods calls [`handle_alloc_error`] method
    /// (unless "alloc" feature is not enabled, in this case it panics).
    /// Non-prefixed methods require "oom_handling" feature (enabled by default)
    /// and absence of `no_global_oom_handling` cfg.
    ///
    /// [`Blink`] can be reset by calling `reset` method.
    /// It drops all emplaced values and resets associated allocator instance.
    /// If allocator instance is shared, resetting it will have no effect.
    ///
    /// [`handle_alloc_error`]: alloc::alloc::handle_alloc_error
    /// [`allocation_api`]: https://doc.rust-lang.org/beta/unstable-book/library-features/allocator-api.html
    pub struct Blink<A = +BlinkAlloc<Global>> {
        drop_list: DropList,
        alloc: A,
    }
}

impl<A> Default for Blink<A>
where
    A: Default,
{
    fn default() -> Self {
        Blink::new_in(Default::default())
    }
}

#[cfg(feature = "alloc")]
impl Blink<BlinkAlloc<Global>> {
    /// Creates new blink instance with `BlinkAlloc` baked by `Global`
    /// allocator.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(not(feature = "oom_handling"))] fn main() {}
    /// # #[cfg(feature = "oom_handling")] fn main() {
    /// use blink_alloc::Blink;
    /// let mut blink = Blink::new();
    ///
    /// blink.emplace().value(42);
    /// # }
    /// ```
    pub const fn new() -> Self {
        Blink::new_in(BlinkAlloc::new())
    }

    /// Creates new blink instance with `BlinkAlloc` baked by `Global`
    /// allocator.
    /// `BlinkAlloc` receives starting chunk size.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(not(feature = "oom_handling"))] fn main() {}
    /// # #[cfg(feature = "oom_handling")] fn main() {
    /// use blink_alloc::Blink;
    /// let mut blink = Blink::with_chunk_size(16);
    ///
    /// blink.emplace().value(42);
    /// # }
    pub const fn with_chunk_size(capacity: usize) -> Self {
        Blink::new_in(BlinkAlloc::with_chunk_size(capacity))
    }
}

impl<A> Blink<A> {
    /// Creates new blink instance with provided allocator instance.
    pub const fn new_in(alloc: A) -> Self {
        Blink {
            drop_list: DropList::new(),
            alloc,
        }
    }

    /// Returns reference to allocator instance.
    pub fn inner(&self) -> &A {
        &self.alloc
    }

    /// Drops all allocated values.
    ///
    /// Prefer to use `reset` method if associated allocator instance supports it.
    pub fn drop_all(&mut self) {
        self.drop_list.reset();
    }
}

impl<A> Blink<A>
where
    A: BlinkAllocator,
{
    /// Drops all allocated values.
    /// And resets associated allocator instance.
    pub fn reset(&mut self) {
        self.drop_list.reset();
        self.alloc.reset();
    }

    /// Allocates memory for a copy of the slice.
    /// If allocation fails, returns `Err`.
    /// Otherwise copies the slice into the allocated memory and returns
    /// mutable reference to the copy.
    #[inline]
    unsafe fn _try_slice_copy<'a, T, E>(
        &'a self,
        slice: &[T],
        alloc_err: impl FnOnce(Layout) -> E,
    ) -> Result<&'a mut [T], E>
    where
        T: Copy,
    {
        let layout = Layout::for_value(slice);
        let Ok(ptr) = self.alloc.allocate(layout) else {
            return Err(alloc_err(layout));
        };

        let ptr = ptr.as_ptr().cast();
        core::ptr::copy_nonoverlapping(slice.as_ptr(), ptr, slice.len());
        Ok(core::slice::from_raw_parts_mut(ptr, slice.len()))
    }

    unsafe fn _try_emplace_drop<'a, T, I, G: 'a, E>(
        &'a self,
        init: I,
        f: impl FnOnce(&mut EmplaceSlot<T, G>, I),
        err: impl FnOnce(G) -> E,
        alloc_err: impl FnOnce(I, Layout) -> E,
    ) -> Result<&'a mut T, E> {
        let layout = Layout::new::<DropItem<Result<T, ManuallyDrop<E>>>>();

        let Ok(ptr) = self.alloc.allocate(layout) else {
            return Err(alloc_err(init, layout));
        };

        // Safety: `item_ptr` is a valid pointer to allocated memory for type `DropItem<T>`.
        let item = unsafe { DropItem::init_value(ptr.cast(), init, f) };

        if item.value.is_ok() {
            match self.drop_list.add(item) {
                Ok(value) => return Ok(value),
                _ => unreachable!(),
            }
        }

        match &mut item.value {
            Err(g) => {
                let err = err(unsafe { ManuallyDrop::take(g) });
                // Give memory back.
                self.alloc.deallocate(ptr.cast(), layout);
                Err(err)
            }
            _ => unreachable!(),
        }
    }

    unsafe fn _try_emplace_no_drop<'a, T, I, G: 'a, E>(
        &self,
        init: I,
        f: impl FnOnce(&mut EmplaceSlot<T, G>, I),
        err: impl FnOnce(G) -> E,
        alloc_err: impl FnOnce(I, Layout) -> E,
    ) -> Result<&'a mut T, E> {
        let layout = Layout::new::<T>();
        let Ok(ptr) = self.alloc.allocate(layout) else {
            return Err(alloc_err(init, layout));
        };

        // Safety: `ptr` is a valid pointer to allocated memory.
        // Allocated with this `T`'s layout.
        // Duration of the allocation is until next call to [`BlinkAlloc::reset`].

        let uninit = &mut *ptr.as_ptr().cast();
        f(uninit, init);

        match uninit.assume_init_mut() {
            Ok(value) => Ok(value),
            Err(g) => {
                let err = err(unsafe { ManuallyDrop::take(g) });
                // Give memory back.
                self.alloc.deallocate(ptr.cast(), layout);
                Err(err)
            }
        }
    }

    /// Allocates memory for a value and emplaces value into the memory
    /// using init value and provided closure.
    /// If allocation fails, returns `Err(init)`.
    /// Otherwise calls closure consuming `init`
    /// and initializes memory with closure result.
    #[inline]
    unsafe fn _try_emplace<'a, T, I, G: 'a, E>(
        &'a self,
        init: I,
        f: impl FnOnce(&mut EmplaceSlot<T, G>, I),
        no_drop: bool,
        err: impl FnOnce(G) -> E,
        alloc_err: impl FnOnce(I, Layout) -> E,
    ) -> Result<&'a mut T, E> {
        if !needs_drop::<T>() || no_drop {
            self._try_emplace_no_drop(init, f, err, alloc_err)
        } else {
            self._try_emplace_drop(init, f, err, alloc_err)
        }
    }

    unsafe fn _try_emplace_drop_from_iter<'a, T: 'a, I, E>(
        &'a self,
        mut iter: I,
        err: impl FnOnce(&'a mut [T], T, Option<Layout>) -> E,
    ) -> Result<&'a mut [T], E>
    where
        I: Iterator<Item = T>,
    {
        struct Guard<'a, T: 'a> {
            ptr: Option<NonNull<DropItem<[T; 0]>>>,
            count: usize,
            drop_list: &'a DropList,
        }

        impl<'a, T> Drop for Guard<'a, T> {
            #[inline]
            fn drop(&mut self) {
                self._flush();
            }
        }

        impl<'a, T> Guard<'a, T> {
            #[inline]
            fn set(mut self) -> &'a mut [T] {
                self._flush()
            }

            #[inline]
            fn _flush(&mut self) -> &'a mut [T] {
                if let Some(ptr) = self.ptr.take() {
                    // Safety: `item` was properly initialized.
                    let (item, slice) = unsafe { DropItem::init_slice(ptr, self.count) };
                    unsafe {
                        self.drop_list.add(item);
                    }
                    slice
                } else {
                    &mut []
                }
            }
        }

        let mut guard = Guard {
            ptr: None,
            count: 0,
            drop_list: &self.drop_list,
        };

        let mut last_layout = Layout::new::<()>();
        let item_layout = Layout::new::<DropItem<[T; 0]>>();

        loop {
            let Some(one_more_elem) = iter.next() else {
                return Ok(guard.set());
            };

            let (lower, _) = iter.size_hint();
            let Some(lower) = lower.checked_add(1) else {
                return Err(err(guard.set(), one_more_elem, None));
            };
            let Some(mut size_hint) = guard.count.checked_add(lower) else {
                return Err(err(guard.set(), one_more_elem, None));
            };
            let Ok(array_layout) = Layout::array::<T>(size_hint) else {
                return Err(err(guard.set(), one_more_elem, None));
            };
            let Ok((full_layout, array_offset)) = item_layout.extend(array_layout) else {
                return Err(err(guard.set(), one_more_elem, None));
            };

            debug_assert_eq!(array_offset, size_of::<DropItem<[T; 0]>>());

            let res = match guard.ptr {
                None => self.alloc.allocate(full_layout),
                Some(ptr) => self.alloc.grow(ptr.cast(), last_layout, full_layout),
            };

            let Ok(ptr) = res else {
                return Err(err(guard.set(), one_more_elem, Some(full_layout)));
            };
            last_layout = full_layout;

            let item_ptr = ptr.cast();
            guard.ptr = Some(item_ptr);

            let len = ptr.len();
            if len > full_layout.size() {
                let fits = (len - size_of::<DropItem<[T; 0]>>()) / size_of::<T>();
                size_hint = fits;
            } else {
                debug_assert_eq!(len, full_layout.size());
            }

            let array_ptr = unsafe { item_ptr.as_ptr().add(1).cast::<T>() };

            // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
            // And `hint` is larger than `guard.count`
            unsafe {
                ptr::write(array_ptr.add(guard.count), one_more_elem);
            }

            guard.count += 1;

            for idx in guard.count..size_hint {
                if Layout::new::<Option<T>>() == Layout::new::<T>() {
                    // Putting elements directly into the array.
                    if in_place(array_ptr.add(idx).cast(), &mut iter, Iterator::next).is_none() {
                        break;
                    }
                } else {
                    match iter.next() {
                        None => break,
                        Some(elem) => ptr::write(array_ptr.add(idx), elem),
                    }
                }
                guard.count = idx + 1;
            }
        }
    }

    unsafe fn _try_emplace_no_drop_from_iter<'a, T: 'a, I, E>(
        &'a self,
        mut iter: I,
        err: impl FnOnce(&'a mut [T], T, Option<Layout>) -> E,
    ) -> Result<&'a mut [T], E>
    where
        I: Iterator<Item = T>,
    {
        struct Guard<T> {
            ptr: Option<NonNull<T>>,
            count: usize,
        }

        impl<T> Guard<T> {
            #[inline]
            fn set<'a>(self) -> &'a mut [T] {
                match self.ptr {
                    Some(ptr) => {
                        // Safety: `slice` was properly initialized.
                        unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), self.count) }
                    }
                    None => &mut [],
                }
            }
        }

        let mut guard = Guard {
            ptr: None,
            count: 0,
        };
        let mut last_layout = Layout::new::<()>();

        loop {
            let Some(one_more_elem) = iter.next() else {
                return Ok(guard.set());
            };

            let (lower, _) = iter.size_hint();
            let Some(lower) = lower.checked_add(1) else {
                return Err(err(guard.set(), one_more_elem, None));
            };
            let Some(mut size_hint) = guard.count.checked_add(lower) else {
                return Err(err(guard.set(), one_more_elem, None));
            };
            let Ok(array_layout) = Layout::array::<T>(size_hint) else {
                return Err(err(guard.set(), one_more_elem, None));
            };

            let res = match guard.ptr {
                None => self.alloc.allocate(array_layout),
                Some(ptr) => self.alloc.grow(ptr.cast(), last_layout, array_layout),
            };

            let Ok(ptr) = res else {
                return Err(err(guard.set(), one_more_elem, Some(array_layout)));
            };
            last_layout = array_layout;

            guard.ptr = Some(ptr.cast());

            let len = ptr.len();
            if len > array_layout.size() {
                let fits = len / size_of::<T>();
                debug_assert!(fits >= size_hint);
                size_hint = fits;
            } else {
                debug_assert_eq!(len, array_layout.size());
            }

            let array_ptr = ptr.as_ptr().cast::<T>();
            // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
            // And `hint` is larger than `guard.count`
            unsafe {
                ptr::write(array_ptr.add(guard.count), one_more_elem);
            }

            guard.count += 1;

            for idx in guard.count..size_hint {
                if Layout::new::<Option<T>>() == Layout::new::<T>() {
                    // Putting elements directly into the array.
                    if in_place(array_ptr.add(idx).cast(), &mut iter, Iterator::next).is_none() {
                        break;
                    }
                } else {
                    match iter.next() {
                        None => break,
                        Some(elem) => ptr::write(array_ptr.add(idx), elem),
                    }
                }
                guard.count = idx + 1;
            }
        }
    }

    /// Allocates memory for a value and emplaces value into the memory
    /// using init value and provided closure.
    /// If allocation fails, returns `Err(init)`.
    /// Otherwise calls closure consuming `init`
    /// and initializes memory with closure result.
    #[inline]
    unsafe fn _try_emplace_from_iter<'a, T: 'a, I, E>(
        &'a self,
        iter: I,
        no_drop: bool,
        err: impl FnOnce(&'a mut [T], T, Option<Layout>) -> E,
    ) -> Result<&mut [T], E>
    where
        I: IntoIterator<Item = T>,
    {
        if !needs_drop::<T>() || no_drop {
            self._try_emplace_no_drop_from_iter(iter.into_iter(), err)
        } else {
            self._try_emplace_drop_from_iter(iter.into_iter(), err)
        }
    }
}

/// Provides interface for emplacing values.
/// Created by [`Blink::emplace`], [`Blink::emplace_no_drop`]
/// and [`Blink::emplace_unchecked`].
pub struct Emplace<'a, A, T, R = &'a mut T, S = &'a mut [T]> {
    blink: &'a Blink<A>,
    no_drop: bool,
    marker: PhantomData<fn(T) -> (R, S)>,
}

impl<'a, A, T, R, S> Emplace<'a, A, T, R, S>
where
    A: BlinkAllocator,
    T: 'a,
    R: CoerceFromMut<'a, T>,
    S: CoerceFromMut<'a, [T]>,
{
    /// Allocates memory for a value and moves `value` into the memory.
    /// If allocation fails, returns `Err(value)`.
    /// On success returns reference to the emplaced value.
    #[inline]
    pub fn try_value(&self, value: T) -> Result<R, T> {
        unsafe {
            self.blink._try_emplace(
                value,
                |slot, value| {
                    slot.write(Ok::<_, ManuallyDrop<Infallible>>(value));
                },
                self.no_drop,
                |never| match never {},
                |init, _| init,
            )
        }
        .map(R::coerce)
    }

    /// Allocates memory for a value and moves `value` into the memory.
    /// Returns reference to the emplaced value.
    /// If allocation fails, diverges.
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    pub fn value(&self, value: T) -> R {
        R::coerce(
            unsafe {
                self.blink._try_emplace(
                    value,
                    |slot, value| {
                        slot.write(Ok::<_, ManuallyDrop<Infallible>>(value));
                    },
                    self.no_drop,
                    identity,
                    |_, layout| handle_alloc_error(layout),
                )
            }
            .safe_ok(),
        )
    }

    /// Allocates memory for a value.
    /// On success invokes closure and initialize the value.
    /// Returns reference to the value.
    /// If allocation fails, returns error with closure.
    #[inline]
    pub fn try_with<F>(&self, f: F) -> Result<R, F>
    where
        F: FnOnce() -> T,
    {
        unsafe {
            self.blink._try_emplace(
                f,
                |slot, f| {
                    slot.write(Ok::<_, ManuallyDrop<Infallible>>(f()));
                },
                self.no_drop,
                never,
                |f, _| f,
            )
        }
        .map(R::coerce)
    }

    /// Allocates memory for a value.
    /// On success invokes closure and initialize the value.
    /// Returns reference to the value.
    /// If allocation fails, diverges.
    ///
    /// This version works only for `'static` values.
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    pub fn with<F>(&self, f: F) -> R
    where
        F: FnOnce() -> T,
    {
        R::coerce(
            unsafe {
                self.blink._try_emplace(
                    f,
                    |slot, f| {
                        slot.write(Ok::<_, ManuallyDrop<Infallible>>(f()));
                    },
                    self.no_drop,
                    never,
                    |_, layout| handle_alloc_error(layout),
                )
            }
            .safe_ok(),
        )
    }
    /// Allocates memory for a value.
    /// If allocation fails, returns error with closure.
    /// On success invokes closure and initialize the value.
    /// If closure fails, returns the error.
    /// Returns reference to the value.
    #[inline]
    pub fn try_with_fallible<F, E>(&self, f: F) -> Result<R, Result<E, F>>
    where
        F: FnOnce() -> Result<T, E>,
        E: 'a,
    {
        unsafe {
            self.blink._try_emplace(
                f,
                |slot, f| {
                    slot.write(f().map_err(ManuallyDrop::new));
                },
                self.no_drop,
                |err| Ok(err),
                |f, _| Err(f),
            )
        }
        .map(R::coerce)
    }

    /// Allocates memory for a value.
    /// If allocation fails, returns error with closure.
    /// On success invokes closure and initialize the value.
    /// If closure fails, returns the error.
    /// Returns reference to the value.
    #[inline]
    pub fn with_fallible<F, E>(&self, f: F) -> Result<R, E>
    where
        F: FnOnce() -> Result<T, E>,
        E: 'a,
    {
        unsafe {
            self.blink._try_emplace(
                f,
                |slot, f| {
                    slot.write(f().map_err(ManuallyDrop::new));
                },
                self.no_drop,
                identity,
                |_, layout| handle_alloc_error(layout),
            )
        }
        .map(R::coerce)
    }

    /// Allocates memory for an array and initializes it with
    /// values from iterator.
    /// Uses iterator hints to allocate memory.
    /// If iterator yields more values than allocated array can hold,
    /// new larger array is allocated and values are copied.
    /// Repeats until iterator is exhausted.
    /// Works best on iterators that report accurate upper size hint.
    /// Grows allocated memory potentially reducing number of allocations
    /// and copies.
    /// If allocation fails, returns slice of values emplaced so far.
    /// And one element that was taken from iterator and not emplaced.
    #[inline]
    pub fn try_from_iter<I>(&self, iter: I) -> Result<S, (S, T)>
    where
        I: IntoIterator<Item = T>,
    {
        unsafe {
            self.blink
                ._try_emplace_from_iter(iter, self.no_drop, |slice: &'a mut [T], value, _| {
                    (S::coerce(slice), value)
                })
        }
        .map(S::coerce)
    }

    /// Allocates memory for an array and initializes it with
    /// values from iterator.
    /// Uses iterator hints to allocate memory.
    /// If iterator yields more values than allocated array can hold,
    /// new larger array is allocated and values are copied.
    /// Repeats until iterator is exhausted.
    /// Works best on iterators that report accurate upper size hint.
    /// Grows allocated memory potentially reducing number of allocations
    /// and copies.
    /// If allocation fails, diverges.
    /// Values already emplaced will be dropped.
    /// One last value that was taken from iterator and not emplaced
    /// is dropped before this method returns.
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    pub fn from_iter<I>(&self, iter: I) -> S
    where
        I: Iterator<Item = T>,
    {
        S::coerce(
            unsafe {
                self.blink
                    ._try_emplace_from_iter(iter, self.no_drop, |_, _, layout| match layout {
                        Some(layout) => handle_alloc_error(layout),
                        None => size_overflow(),
                    })
            }
            .safe_ok(),
        )
    }
}

impl<A> Blink<A>
where
    A: BlinkAllocator,
{
    /// Puts value into this `Blink` instance.
    /// Returns reference to the value.
    ///
    /// Effectively extends lifetime of the value
    /// from local scope to the reset scope.
    ///
    /// For more flexible value placement see
    /// [`Blink::emplace`], [`Blink::emplace_no_drop`] and
    /// [`Blink::emplace_unchecked`].
    ///
    /// # Example
    ///
    /// ```
    /// # use blink_alloc::Blink;
    /// let mut blink = Blink::new();
    /// let foo = blink.put(42);
    /// assert_eq!(*foo, 42);
    /// *foo = 24;
    /// blink.reset();
    /// // assert_eq!(*foo, 24); // Cannot compile. `foo` does not outlive reset.
    /// ```
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    pub fn put<T: 'static>(&self, value: T) -> &mut T {
        self.emplace().value(value)
    }

    /// Allocates memory for a value.
    /// Returns some reference to the uninitialized value.
    /// If allocation fails, returns none.
    #[inline]
    pub fn try_uninit<T>(&self) -> Option<&mut MaybeUninit<T>> {
        let layout = Layout::new::<T>();
        let ptr = self.alloc.allocate(layout).ok()?;

        // Safety:
        // - `ptr` is valid for `layout`.
        // - `MaybeUninit` is always initialized.
        Some(unsafe { &mut *ptr.as_ptr().cast() })
    }

    /// Allocates memory for a value.
    /// Returns reference to the uninitialized value.
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub fn uninit<T>(&self) -> &mut MaybeUninit<T> {
        let layout = Layout::new::<T>();
        let ptr = self
            .alloc
            .allocate(layout)
            .unwrap_or_else(|_| handle_alloc_error(layout));

        // Safety:
        // - `ptr` is valid for `layout`.
        // - `MaybeUninit` is always initialized.
        unsafe { &mut *ptr.as_ptr().cast() }
    }

    /// Copies the slice to the allocated memory
    /// and returns reference to the new slice.
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub fn slice_copy<T>(&self, slice: &[T]) -> &mut [T]
    where
        T: Copy,
    {
        let result = unsafe { self._try_slice_copy(slice, handle_alloc_error) };
        match result {
            Ok(slice) => slice,
            Err(never) => never,
        }
    }

    /// Allocates memory for a copy of the slice.
    /// Copies the slice to the allocated memory
    /// and returns reference to the new slice.
    /// If allocation fails, returns `None`.
    #[inline]
    pub fn try_slice_copy<T>(&self, slice: &[T]) -> Option<&mut [T]>
    where
        T: Copy,
    {
        unsafe { self._try_slice_copy(slice, |_| ()) }.ok()
    }

    /// Copies the slice to the allocated memory
    /// and returns reference to the new slice.
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub fn str_copy(&self, string: &str) -> &mut str {
        let result = unsafe { self._try_slice_copy(string.as_bytes(), handle_alloc_error) };
        match result {
            Ok(slice) => unsafe { core::str::from_utf8_unchecked_mut(slice) },
            Err(never) => never,
        }
    }

    /// Allocates memory for a copy of the slice.
    /// Copies the slice to the allocated memory
    /// and returns reference to the new slice.
    /// If allocation fails, returns `None`.
    #[inline]
    pub fn try_str_copy(&self, string: &str) -> Option<&mut str> {
        unsafe { self._try_slice_copy(string.as_bytes(), |_| ()) }
            .ok()
            .map(|bytes| unsafe { core::str::from_utf8_unchecked_mut(bytes) })
    }

    /// Returns an `Emplace` adaptor that can emplace values into
    /// the blink allocator.
    ///
    /// This version requires the value type to be `'static`.
    /// To use with non-static types consider using one of the following:
    ///
    /// * [`Blink::emplace_no_drop`]
    ///   Causes emplaced value to not be dropped on reset.
    ///   Avoiding potential unsoundness in `Drop` implementation.
    /// * [`Blink::emplace_shared`]
    ///   Returns shared reference to emplaced values.
    /// * [`Blink::emplace_unchecked`]
    ///   Unsafe version of `emplace`.
    ///   User must guarantee that the value won't have access to references
    ///   allocated by the blink allocator later.
    ///
    /// # Example
    ///
    /// ```
    /// # use blink_alloc::Blink;
    /// let mut blink = Blink::new();
    /// let foo = blink.emplace().value(42);
    /// assert_eq!(*foo, 42);
    /// *foo = 24;
    /// blink.reset();
    /// // assert_eq!(*foo, 24); // Cannot compile. `foo` does not outlive reset.
    /// ```
    #[inline]
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
    /// * [`Blink::emplace_shared`]
    ///   Returns shared reference to emplaced values.
    /// * [`Blink::emplace_unchecked`]
    ///   Unsafe version of `emplace`.
    ///   User must guarantee that the value won't have access to references
    ///   allocated by the blink allocator later.
    ///
    /// # Example
    ///
    /// ```
    /// # use blink_alloc::Blink;
    /// struct Foo<'a>(&'a String);
    ///
    /// impl Drop for Foo<'_> {
    ///     fn drop(&mut self) {
    ///         println!("{}", self.0);
    ///     }
    /// }
    ///
    /// let mut blink = Blink::new();
    /// let s = "Hello".to_owned();
    /// let foo = blink.emplace_no_drop().value(Foo(&s));
    /// assert_eq!(foo.0, "Hello");
    /// let world = blink.put("World".to_owned());
    /// // Would be unsound if `foo` could be dropped.
    /// foo.0 = world;
    /// blink.reset();
    /// // assert_eq!(foo.0, "Universe"); // Cannot compile. `foo` does not outlive reset.
    /// ```
    #[inline]
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
    /// This version returns shared references to emplaced values.
    /// Lifts the `'static` requirement.
    /// Still allows emplaced values to be dropped on reset.
    ///
    /// To drop returned value on reset, consider one of the following:
    ///
    /// * [`Blink::emplace`]
    ///   Requires the value type to be `'static`.
    /// * [`Blink::emplace_no_drop`]
    ///   Causes emplaced value to not be dropped on reset.
    ///   Avoiding potential unsoundness in `Drop` implementation.
    /// * [`Blink::emplace_unchecked`]
    ///   Unsafe version of `emplace`.
    ///   User must guarantee that the value won't have access to references
    ///   allocated by the blink allocator later.
    ///
    ///
    /// ```
    /// # use blink_alloc::Blink;
    /// struct Foo<'a>(&'a String);
    ///
    /// impl Drop for Foo<'_> {
    ///     fn drop(&mut self) {
    ///         println!("{}", self.0);
    ///     }
    /// }
    ///
    /// let mut blink = Blink::new();
    /// let s = "Hello".to_owned();
    /// let foo = blink.emplace_no_drop().value(Foo(&s));
    /// assert_eq!(foo.0, "Hello");
    /// let world = blink.put("World".to_owned());
    /// // Would be unsound if `foo` was mutable.
    /// // foo.0 = world;
    /// blink.reset();
    /// // assert_eq!(foo.0, "Universe"); // Cannot compile. `foo` does not outlive reset.
    /// ```
    #[inline]
    pub fn emplace_shared<T>(&self) -> Emplace<A, T, &T, &[T]> {
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
    /// * [`Blink::emplace_shared`]
    ///   Returns shared reference to emplaced values.
    ///
    /// # Safety
    ///
    /// Avoid incorrect usage. See below.
    ///
    /// # Incorrect usage example
    ///
    /// Other emplace methods are safe as they guarantee following case
    /// is impossible.
    ///
    /// ```no_run
    /// # use blink_alloc::Blink;
    /// struct Foo<'a>(&'a String);
    ///
    /// impl Drop for Foo<'_> {
    ///     fn drop(&mut self) {
    ///         println!("{}", self.0);
    ///     }
    /// }
    ///
    /// let mut blink = Blink::new();
    /// let s = "Hello".to_owned();
    /// let foo = blink.emplace_no_drop().value(Foo(&s));
    /// assert_eq!(foo.0, "Hello");
    /// let world = blink.put("World".to_owned());
    /// // Unsound since `foo` would access `world` in `Drop`
    /// // and `world` is dropped earlier.
    /// foo.0 = world;
    /// blink.reset();
    /// // assert_eq!(foo.0, "Universe"); // Cannot compile. `foo` does not outlive reset.
    /// ```
    #[inline]
    pub unsafe fn emplace_unchecked<T>(&self) -> Emplace<A, T> {
        Emplace {
            blink: self,
            no_drop: false,
            marker: PhantomData,
        }
    }
}

/// Wrapper for [`Blink`] that implements [`Send`].
///
/// Normally it is impossible to send [`Blink`] to another thread
/// due to the fact that it will drop non-sendable types on reset.
///
/// This wrapper resets [`Blink`] on construction and thus safe to send.
///
/// # Example
///
/// ```
/// # use blink_alloc::{SendBlink, Blink};
/// let mut blink = Blink::new();
/// let rc = std::rc::Rc::new(42);
/// let rc = blink.put(rc);
/// assert_eq!(**rc, 42);
/// let send_blink = SendBlink::new(blink);
///
/// std::thread::scope(move |_| {
///     let mut blink = send_blink.into_inner();
///     blink.put(42);
/// });
/// ````
pub struct SendBlink<A> {
    blink: Blink<A>,
}

impl<A> SendBlink<A>
where
    A: BlinkAllocator,
{
    /// Creates new [`SendBlink`] from [`Blink`].
    /// Resets the blink allocator to avoid dropping non-sendable types on other threads.
    pub fn new(mut blink: Blink<A>) -> Self {
        blink.reset();
        SendBlink { blink }
    }

    /// Returns inner [`Blink`] value.
    pub fn into_inner(self) -> Blink<A> {
        self.blink
    }
}

#[inline]
fn never<T>(never: Infallible) -> T {
    match never {}
}
