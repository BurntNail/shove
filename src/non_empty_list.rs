use std::alloc::{alloc, dealloc, realloc, Layout};
use std::fmt::{Debug, Formatter};
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ptr::NonNull;
use std::{ptr, slice, vec};
use std::mem::ManuallyDrop;

///list that cannot be empty, and is push-only
pub struct NonEmptyList<T> {
    len: NonZeroUsize,
    cap: NonZeroUsize,
    ptr: NonNull<T>,
    _pd: PhantomData<T>
}

unsafe impl<T> Send for NonEmptyList<T> {}
unsafe impl<T> Sync for NonEmptyList<T> {}

impl<T> NonEmptyList<T> {
    pub fn new (list: Vec<T>) -> Option<Self> {
        if list.is_empty() {
            None
        } else {
            //safety: just checked :)
            Some(unsafe {
                Self::from_non_empty_vec(list)
            })
        }
    }

    ///# Safety
    /// Passed in list must not be empty.
    pub unsafe fn from_non_empty_vec (list: Vec<T>) -> Self {
        let mut list = ManuallyDrop::new(list);

        let len = NonZeroUsize::new_unchecked(list.len());
        let cap = NonZeroUsize::new_unchecked(list.capacity());
        let ptr = NonNull::new_unchecked(list.as_mut_ptr());

        Self {
            len, cap, ptr, _pd: PhantomData
        }

        // let mut list = ManuallyDrop::new(list);
        //
        // let len = NonZeroUsize::new_unchecked(list.len());
        // let cap = NonZeroUsize::new_unchecked(list.capacity());
        // let layout = Layout::array::<T>(cap.get()).expect("unable to get layout");
        // let ptr = NonNull::new(alloc(layout) as *mut T).expect("unable to allocate");
        //
        // copy_nonoverlapping(list.as_ptr(), ptr.as_ptr(), len.get());
        // dealloc(list.as_mut_ptr() as *mut u8, layout);
        //
        // Self {
        //     len,
        //     cap,
        //     ptr,
        //     _pd: PhantomData
        // }
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

        let new_cap = match min_cap.checked_next_power_of_two() {
            Some(x) => if fails_constraints(x) {
                min_cap
            } else {
                x
            },
            None => min_cap
        }.get();

        let new_ptr = NonNull::new(unsafe {
            realloc(self.ptr.as_ptr() as *mut u8, old_layout, new_cap * size_of::<T>()) as *mut T
        }).expect("reallocation of new empty list failed");

        self.ptr = new_ptr;
        self.cap = unsafe {
            NonZeroUsize::new_unchecked(new_cap)
        };
    }

    pub fn push (&mut self, el: T) {
        if self.len >= self.cap {
            self.grow_at_least(1);
        }

        debug_assert!(self.len < self.cap);

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
            //gotta love unreliable size hints
            if self.len == self.cap {
                self.grow_at_least(1);
            }

            debug_assert!(self.len < self.cap);

            unsafe {
                self.ptr.add(self.len.get()).write(el);
                self.len = self.len.checked_add(1).unwrap_unchecked();
            }
        }
    }
}

impl<T> From<NonEmptyList<T>> for Vec<T> {
    fn from(value: NonEmptyList<T>) -> Self {
        //safety: takes ownership of the NEL
        unsafe {
            Vec::from_raw_parts(value.ptr.as_ptr(), value.len.get(), value.cap.get())
        }
    }
}

impl<T> Drop for NonEmptyList<T> {
    fn drop(&mut self) {
        unsafe {
            //drop all individual elements
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(self.ptr.as_ptr(), self.len.get()));

            let layout = Layout::array::<T>(self.cap.get()).expect("used this alloc to get the allocation");
            dealloc(self.ptr.as_ptr() as *mut u8, layout);
        }
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
        let layout = Layout::array::<T>(self.cap.get()).expect("unable to create layout for cloning NonEmptyList");
        let ptr = unsafe {
            NonNull::new(alloc(layout) as *mut T).expect("unable to allocate for new NonEmptyList")
        };

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
            cap: self.cap,
            _pd: PhantomData
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

impl<T> IntoIterator for NonEmptyList<T> {
    type Item = T;
    type IntoIter = vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        let v: Vec<T> = self.into();
        v.into_iter()
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

//gotta love some chatgpt tests :))))))
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_non_empty_list_from_vec() {
        let vec = vec![1, 2, 3];
        let non_empty_list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        assert_eq!(non_empty_list.as_ref(), &[1, 2, 3]);
    }

    #[test]
    fn test_push() {
        let vec = vec![1];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list.push(2);
        list.push(3);

        assert_eq!(list.as_ref(), &[1, 2, 3]);
    }

    #[test]
    fn test_extend() {
        let vec = vec![1];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list.extend(vec![2, 3, 4]);

        assert_eq!(list.as_ref(), &[1, 2, 3, 4]);
    }

    #[test]
    fn test_as_ref_and_as_mut() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        assert_eq!(list.as_ref(), &[1, 2, 3]);

        list.as_mut()[1] = 42;
        assert_eq!(list.as_ref(), &[1, 42, 3]);
    }

    #[test]
    fn test_clone() {
        let vec = vec![1, 2, 3];
        let list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let cloned_list = list.clone();
        assert_eq!(list.as_ref(), cloned_list.as_ref());
    }

    #[test]
    fn test_debug() {
        let vec = vec![1, 2, 3];
        let list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        assert_eq!(format!("{:?}", list), "[1, 2, 3]");
    }

    #[test]
    fn test_non_empty_list_builder() {
        let non_empty_list = NonEmptyList::new(vec![1, 2, 3]).unwrap();

        assert_eq!(non_empty_list.as_ref(), &[1, 2, 3]);
    }

    #[test]
    fn test_non_empty_list_builder_empty() {
        let builder: NonEmptyListBuilder<i32> = NonEmptyListBuilder(vec![]);
        let result = NonEmptyList::try_from(builder);

        assert!(result.is_err());

        let result2 = NonEmptyList::new(Vec::<i32>::new());
        assert!(result2.is_none());
    }

    #[test]
    #[should_panic(expected = "unable to create large enough list")]
    fn test_grow_beyond_limits() {
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec![1]) };

        list.grow_at_least(usize::MAX); // Should panic due to memory constraints
    }

    #[test]
    fn test_large_extend() {
        let vec = vec![1];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let large_iter = (2..10_000).collect::<Vec<_>>();
        list.extend(large_iter);

        assert_eq!(list.len.get(), 10_000 - 1);
        assert_eq!(list.as_ref()[0], 1);
        assert_eq!(list.as_ref()[1], 2);
    }
}
