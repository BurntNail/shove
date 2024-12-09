use std::alloc::{alloc, realloc, Layout};
use std::fmt::{Debug, Formatter};
use std::num::NonZeroUsize;
use std::ptr::NonNull;
use std::slice;

///list that cannot be empty, and is push-only
pub struct NonEmptyList<T> {
    len: NonZeroUsize,
    cap: NonZeroUsize,
    ptr: NonNull<T>
}

impl<T> NonEmptyList<T> {
    ///# Safety
    /// Passed in list must not be empty.
    pub unsafe fn from_non_empty_vec (list: Vec<T>) -> Self {
        let len = NonZeroUsize::new_unchecked(list.len());
        let cap = NonZeroUsize::new_unchecked(list.capacity());
        let ptr = list.leak().as_mut_ptr();
        let ptr = NonNull::new_unchecked(ptr);

        Self {
            len, cap, ptr
        }
    }

    fn grow_at_least (&mut self, extra: usize) {
        let old_layout = Layout::array::<T>(self.cap.get()).unwrap(); //we allocated with it once lol

        let fails_constraints = |cap: NonZeroUsize| -> bool {
            let actual_size = cap.get() * size_of::<T>();
            const TRUE_MAX: usize = isize::MAX as usize;
            actual_size > TRUE_MAX || {
                let multiple = old_layout.align();

                let number_to_multiply_by = (actual_size as f64 / multiple as f64).ceil() as usize;
                multiple * number_to_multiply_by > TRUE_MAX
            }
        };

        let min_cap = self.cap.checked_add(extra).expect("unable to create large enough list for NonEmptyList");
        if fails_constraints(min_cap) {
            panic!("New list is too large to be a proper vec");
        }

        let new_cap = match self.cap.checked_next_power_of_two() {
            Some(x) => if fails_constraints(x) {
                min_cap
            } else {
                x
            },
            None => min_cap
        }.get();


        let new_ptr = NonNull::new(unsafe {
            realloc(self.ptr.as_ptr(), old_layout, new_cap)
        }).expect("reallocation of new empty list failed") as NonNull<T>;

        self.ptr = new_ptr;
        self.cap = unsafe {
            NonZeroUsize::new_unchecked(new_cap)
        };

    }

    pub fn push (&mut self, el: T) {
        if self.len > self.cap {
            self.grow_at_least(1);
        }

        //safety: we know we've allocated enough
        //safety: we know cap is bigger so len + 1 will fit
        unsafe {
            self.ptr.add(self.len.get()).write(el);
            self.len = self.len.checked_add(1).unwrap_unchecked();
        }
    }

    pub fn extend (&mut self, iter: impl IntoIterator<Item = T>) {
        let iter = iter.into_iter();

        let (min, max) = iter.size_hint();
        match max {
            Some(max) => self.grow_at_least(max),
            None => self.grow_at_least(min),
        }

        for el in iter {
            self.push(el);
        }
    }
}

impl<T> Drop for NonEmptyList<T> {
    fn drop(&mut self) {
        //safety: won't be used again after drop
        let v: Vec<T> = unsafe {
            Vec::from_raw_parts(self.ptr.as_ptr(), self.len.get(), self.cap.get())
        };
        drop(v); //not needed, but nice to see for clarity
    }
}

impl<T> AsRef<[T]> for NonEmptyList<T> {
    fn as_ref(&self) -> &[T] {
        unsafe {
            slice::from_raw_parts(self.ptr.as_ptr() as *const _, self.len.get())
        }
    }
}
impl<T> AsMut<[T]> for NonEmptyList<T> {
    fn as_mut(&mut self) -> &mut [T] {
        unsafe {
            slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len.get())
        }
    }
}

impl<T: Clone> Clone for NonEmptyList<T> {
    fn clone(&self) -> Self {
        unsafe {
            let layout = Layout::array::<T>(self.cap.get()).expect("unable to create layout for cloning NonEmptyList");
            let ptr = NonNull::new(alloc(layout)).expect("unable to allocate for new NonEmptyList") as NonNull<T>;

            //can't use copy_nonoverlapping because we don't know if it does copy
            //can't have a separate impl for copy because no specialisation
            for i in 0..self.len.get() {
                //safety: we know everything up to len is alloc-ed
                unsafe {
                    let this_idx = self.ptr.add(i).as_ref();
                    ptr.add(i).write(this_idx.clone());
                }

            }

            Self {
                ptr,
                len: self.len,
                cap: self.cap
            }
        }
    }
}

impl<T: Debug> Debug for NonEmptyList<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut list = f.debug_list();

        for el in self.as_ref() {
            list.entry(el);
        }

        list.finish()
    }
}

pub struct NonEmptyListBuilder<T>(Vec<T>);

impl<T> TryFrom<NonEmptyListBuilder<T>> for NonEmptyList<T> {
    type Error = NonEmptyListBuilder<T>;

    fn try_from(value: NonEmptyListBuilder<T>) -> Result<Self, Self::Error> {
        if value.0.is_empty() {
            Err(value)
        } else {
            unsafe {
                //safety: just checked for emptiness
                Ok(NonEmptyList::from_non_empty_vec(value.0))
            }
        }
    }
}