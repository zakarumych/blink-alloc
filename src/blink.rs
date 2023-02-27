//! Provides `Blink` allocator adaptor.

use core::{
    alloc::Layout,
    convert::identity,
    marker::PhantomData,
    mem::{needs_drop, size_of},
    ptr::{self, addr_of_mut, copy_nonoverlapping, slice_from_raw_parts_mut, NonNull},
};

use allocator_api2::Allocator;

#[cfg(feature = "alloc")]
use allocator_api2::Global;

use crate::{
    api::BlinkAllocator,
    drop_list::{DropItem, DropList},
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

    unsafe fn _try_emplace_drop<T, I, E>(
        &self,
        init: I,
        f: impl FnOnce(I) -> T,
        err: impl FnOnce(I, Layout) -> E,
    ) -> Result<&mut T, E> {
        let layout = Layout::new::<DropItem<T>>();

        let Ok(ptr) = self.alloc.allocate(layout) else {
            return Err(err(init, layout));
        };

        let ptr = ptr.as_ptr().cast::<u8>();

        let item_ptr = ptr.cast::<DropItem<T>>();
        let value_ptr = addr_of_mut!((*item_ptr).value);

        unsafe {
            ptr::write(value_ptr, f(init));
        }

        // Safety: `item_ptr` is a valid pointer to allocated memory for type `DropItem<T>`.
        let item = unsafe { DropItem::init_value(item_ptr) };
        let reference = self.drop_list.add(item);
        Ok(reference)
    }

    unsafe fn _try_emplace_no_drop<T, I, E>(
        &self,
        init: I,
        f: impl FnOnce(I) -> T,
        err: impl FnOnce(I, Layout) -> E,
    ) -> Result<&mut T, E> {
        let layout = Layout::new::<T>();
        let Ok(ptr) = self.alloc.allocate(layout) else {
            return Err(err(init, layout));
        };

        let ptr = ptr.as_ptr().cast::<T>();

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
    #[inline]
    unsafe fn _try_emplace<T, I, E>(
        &self,
        init: I,
        f: impl FnOnce(I) -> T,
        no_drop: bool,
        err: impl FnOnce(I, Layout) -> E,
    ) -> Result<&mut T, E> {
        if !needs_drop::<T>() || no_drop {
            self._try_emplace_no_drop(init, f, err)
        } else {
            self._try_emplace_drop(init, f, err)
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
            prev_item: Option<NonNull<DropItem<[T]>>>,
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

            let (lower, _) = iter.size_hint();
            let Some(hint) = lower.checked_add(1) else {
                return Err(err(guard.set(), one_more_elem, None));
            };

            let hint = hint.max(prev_count);
            let mut hint = hint.saturating_add(prev_count);

            let item_layout = Layout::new::<DropItem<[T; 0]>>();
            let Ok(array_layout) = Layout::array::<T>(hint) else {
                return Err(err(guard.set(), one_more_elem, None));
            };
            let Ok((full_layout, array_offset)) = item_layout.extend(array_layout) else {
                return Err(err(guard.set(), one_more_elem, None));
            };
            let Ok(ptr) = self.alloc.allocate(full_layout) else {
                return Err(err(guard.set(), one_more_elem, Some(full_layout)));
            };

            let len = ptr.len();
            if len > full_layout.size() {
                let fits = (len - array_offset) / size_of::<T>();
                hint = fits;
            } else {
                debug_assert_eq!(len, full_layout.size());
            }

            let ptr = ptr.as_ptr().cast::<u8>();

            let item_ptr = ptr.cast::<DropItem<[T; 0]>>();
            let array_ptr = unsafe { ptr.add(array_offset).cast::<T>() };

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

    unsafe fn _try_emplace_no_drop_from_iter<'a, T: 'a, I, E>(
        &'a self,
        mut iter: I,
        err: impl FnOnce(&'a mut [T], T, Option<Layout>) -> E,
    ) -> Result<&'a mut [T], E>
    where
        I: Iterator<Item = T>,
    {
        struct State<T> {
            prev_slice: Option<NonNull<[T]>>,
        }

        impl<T> State<T> {
            #[inline]
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

            let (lower, _) = iter.size_hint();
            let Some(hint) = lower.checked_add(1) else {
                return Err(err(state.set(), one_more_elem, None));
            };
            let hint = hint.max(prev_count);
            let mut hint = hint.saturating_add(prev_count);

            let Ok(array_layout) = Layout::array::<T>(hint) else {
                return Err(err(state.set(), one_more_elem, None));
            };
            let Ok(ptr) = self.alloc.allocate(array_layout) else {
                return Err(err(state.set(), one_more_elem, Some(array_layout)));
            };

            let len = ptr.len();
            if len > array_layout.size() {
                let fits = len / size_of::<T>();
                hint = fits;
            } else {
                debug_assert_eq!(len, array_layout.size());
            }

            let array_ptr = ptr.as_ptr().cast::<T>();

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
    /// Returns reference to the emplaced value.
    /// If allocation fails, returns `Err(value)`.
    #[inline]
    pub fn try_value(&self, value: T) -> Result<R, T> {
        unsafe {
            self.blink
                ._try_emplace(value, identity, self.no_drop, |init, _| init)
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
            self.blink
                ._try_emplace(value, identity, self.no_drop, |_, layout| {
                    handle_alloc_error(layout)
                })
        }
        .safe_ok()
        .into()
    }

    /// Allocates memory for a value.
    /// On success invokes closure and initialize the value.
    /// Returns reference to the value.
    /// If allocation fails, returns error with closure.
    #[inline]
    pub fn try_value_with<F>(&self, f: F) -> Result<R, F>
    where
        F: FnOnce() -> T,
    {
        unsafe {
            self.blink
                ._try_emplace(f, call_once, self.no_drop, |f, _| f)
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
    pub fn value_with<F>(&self, f: F) -> R
    where
        F: FnOnce() -> T,
    {
        unsafe {
            self.blink
                ._try_emplace(f, call_once, self.no_drop, |_, layout| {
                    handle_alloc_error(layout)
                })
        }
        .safe_ok()
        .into()
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
fn call_once<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    f()
}
