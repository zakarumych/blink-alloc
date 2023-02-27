//! Provides `Blink` allocator adaptor.

use core::{
    alloc::Layout,
    convert::{identity, Infallible},
    marker::PhantomData,
    mem::{needs_drop, size_of, ManuallyDrop},
    ptr::{self, NonNull},
};

use allocator_api2::Allocator;

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
    for<'a> &'a A: Allocator,
    A: BlinkAllocator,
{
    /// Drops all allocated values.
    /// And resets associated allocator instance.
    pub fn reset(&mut self) {
        self.drop_list.reset();
        self.alloc.reset();
    }

    unsafe fn _try_emplace_drop<'a, T, I, G: 'a, E>(
        &'a self,
        init: I,
        f: impl FnOnce(I) -> Result<T, ManuallyDrop<G>>,
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
        f: impl FnOnce(I) -> Result<T, ManuallyDrop<G>>,
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
        match in_place(ptr.as_ptr().cast(), init, f) {
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
        f: impl FnOnce(I) -> Result<T, ManuallyDrop<G>>,
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
            let Some(hint) = lower.checked_add(1) else {
                return Err(err(guard.set(), one_more_elem, None));
            };

            let hint = hint.max(guard.count);
            let mut hint = hint.saturating_add(guard.count);

            let Ok(array_layout) = Layout::array::<T>(hint) else {
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
                hint = fits;
            } else {
                debug_assert_eq!(len, full_layout.size());
            }

            let array_ptr = unsafe { item_ptr.as_ptr().add(1).cast::<T>() };

            // Safety: `array_ptr` is a valid pointer to allocated memory for type `[T; hint]`.
            // And `hint` is larger than `guard.count`
            unsafe {
                ptr::write(array_ptr.add(guard.count), one_more_elem);
            }

            let mut sub_iter = iter.by_ref().take(hint - guard.count - 1);

            for idx in guard.count + 1.. {
                if Layout::new::<Option<T>>() == Layout::new::<T>() {
                    // Putting elements directly into the array.
                    if in_place(array_ptr.add(idx).cast(), &mut sub_iter, Iterator::next).is_none()
                    {
                        break;
                    }
                } else {
                    match sub_iter.next() {
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
            let Some(hint) = lower.checked_add(1) else {
                return Err(err(guard.set(), one_more_elem, None));
            };
            let hint = hint.max(guard.count);
            let mut hint = hint.saturating_add(guard.count);

            let Ok(array_layout) = Layout::array::<T>(hint) else {
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
                hint = fits;
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

            let mut sub_iter = iter.by_ref().take(hint - guard.count - 1);

            for idx in guard.count.. {
                if Layout::new::<Option<T>>() == Layout::new::<T>() {
                    // Putting elements directly into the array.
                    if in_place(array_ptr.add(idx).cast(), &mut sub_iter, Iterator::next).is_none()
                    {
                        break;
                    }
                } else {
                    match sub_iter.next() {
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
    R: From<&'a mut T>,
    S: From<&'a mut [T]>,
{
    /// Allocates memory for a value and moves `value` into the memory.
    /// If allocation fails, returns `Err(value)`.
    /// On success returns reference to the emplaced value.
    #[inline]
    pub fn try_value(&self, value: T) -> Result<R, T> {
        unsafe {
            self.blink._try_emplace(
                value,
                Ok::<_, ManuallyDrop<Infallible>>,
                self.no_drop,
                |never| match never {},
                |init, _| init,
            )
        }
        .map(R::from)
    }

    /// Allocates memory for a value and moves `value` into the memory.
    /// Returns reference to the emplaced value.
    /// If allocation fails, diverges.
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    pub fn value(&self, value: T) -> R {
        unsafe {
            self.blink._try_emplace(
                value,
                Ok::<_, ManuallyDrop<Infallible>>,
                self.no_drop,
                identity,
                |_, layout| handle_alloc_error(layout),
            )
        }
        .safe_ok()
        .into()
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
                |f| Ok::<_, ManuallyDrop<Infallible>>(f()),
                self.no_drop,
                never,
                |f, _| f,
            )
        }
        .map(R::from)
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
        unsafe {
            self.blink._try_emplace(
                f,
                |f| Ok::<_, ManuallyDrop<Infallible>>(f()),
                self.no_drop,
                never,
                |_, layout| handle_alloc_error(layout),
            )
        }
        .safe_ok()
        .into()
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
                |f| f().map_err(ManuallyDrop::new),
                self.no_drop,
                |err| Ok(err),
                |f, _| Err(f),
            )
        }
        .map(R::from)
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
                |f| f().map_err(ManuallyDrop::new),
                self.no_drop,
                identity,
                |_, layout| handle_alloc_error(layout),
            )
        }
        .map(R::from)
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
    #[inline]
    pub fn try_from_iter<I>(&self, iter: I) -> Result<S, (S, T)>
    where
        I: IntoIterator<Item = T>,
    {
        unsafe {
            self.blink
                ._try_emplace_from_iter(iter, self.no_drop, |slice: &'a mut [T], value, _| {
                    (S::from(slice), value)
                })
        }
        .map(S::from)
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
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    pub fn from_iter<I>(&self, iter: I) -> &'a mut [T]
    where
        I: Iterator<Item = T>,
    {
        unsafe {
            self.blink
                ._try_emplace_from_iter(iter, self.no_drop, |_, _, layout| match layout {
                    Some(layout) => handle_alloc_error(layout),
                    None => size_overflow(),
                })
        }
        .safe_ok()
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
    /// # use blink_alloc   ::Blink;
    /// let mut blink = Blink::new();
    /// let foo = blink.put(42);
    /// assert_eq!(*foo, 42);
    /// *foo = 24;
    /// blink.reset();
    /// // assert_eq!(*foo, 24); // Cannot compile. `foo` cannot outlive reset.
    /// ```
    #[cfg(all(feature = "oom_handling", not(no_global_oom_handling)))]
    #[inline]
    pub fn put<T: 'static>(&self, value: T) -> &mut T {
        self.emplace().value(value)
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
    /// # Incorrect usage example
    ///
    /// Other emplace methods are safe as they guarantee following case
    /// is impossible.
    ///
    /// ```no_run
    /// # #[cfg(not(all(feature = "alloc", feature = "oom_handling")))] fn main() {}
    /// # #[cfg(all(feature = "alloc", feature = "oom_handling"))] fn main() {
    /// # use blink_alloc::Blink;
    ///
    /// struct Foo<'a> {
    ///     s: &'a String,
    /// }
    ///
    /// impl<'a> Drop for Foo<'a> {
    ///     fn drop(&mut self) {
    ///         println!("{}", self.s);
    ///     }
    /// }
    ///
    /// let mut blink = Blink::new();
    /// let s = String::from("Hello");
    /// let foo = unsafe { blink.emplace_unchecked().value(Foo { s: &s }) };
    /// let s = blink.emplace().value(String::from("World"));
    /// foo.s = s;
    /// blink.reset(); // Causes UB when value referenced by `foo` is dropped.
    /// # }
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

#[inline]
fn never<T>(never: Infallible) -> T {
    match never {}
}
