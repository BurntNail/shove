use std::{
    alloc::{alloc, dealloc, realloc, Layout},
    fmt::{Debug, Formatter},
    marker::PhantomData,
    mem::{needs_drop, ManuallyDrop},
    num::NonZeroUsize,
    ops::{Deref, DerefMut, Index, IndexMut},
    ptr::{self, NonNull},
    slice, vec,
};

///list that cannot be empty, and is push-only
pub struct NonEmptyList<T> {
    len: NonZeroUsize,
    cap: NonZeroUsize,
    ptr: NonNull<T>,
    _pd: PhantomData<T>,
}

unsafe impl<T: Send + Sync> Send for NonEmptyList<T> {}
unsafe impl<T: Send + Sync> Sync for NonEmptyList<T> {}

impl<T> NonEmptyList<T> {
    pub fn new(list: Vec<T>) -> Option<Self> {
        if list.is_empty() {
            None
        } else {
            //safety: just checked :)
            Some(unsafe { Self::from_non_empty_vec(list) })
        }
    }

    pub fn single_element(el: T) -> Self {
        if size_of::<T>() == 0 {
            return Self {
                ptr: NonNull::dangling(),
                len: NonZeroUsize::new(1).unwrap(),
                cap: NonZeroUsize::new(usize::MAX).unwrap(),
                _pd: PhantomData,
            };
        }

        let layout = Layout::array::<T>(1).expect("unable to create layout for single element");
        //safety: we know it's uninit and we use the length
        //safety: we know we're not dealing with ZSTs
        let ptr = unsafe {
            NonNull::new(alloc(layout) as *mut T)
                .expect("unable to allocate space for single element")
        };
        debug_assert!(ptr.is_aligned());
        //safety: dst is valid and aligned
        unsafe {
            ptr.write(el);
        }

        let len = NonZeroUsize::new(1).unwrap();
        let cap = NonZeroUsize::new(1).unwrap();

        Self {
            ptr,
            len,
            cap,
            _pd: PhantomData,
        }
    }

    ///# Safety
    /// Passed in list must not be empty.
    pub unsafe fn from_non_empty_vec(list: Vec<T>) -> Self {
        if size_of::<T>() == 0 {
            return Self {
                ptr: NonNull::dangling(),
                len: NonZeroUsize::new(list.len()).unwrap(),
                cap: NonZeroUsize::new(usize::MAX).unwrap(),
                _pd: PhantomData,
            };
        }

        debug_assert!(!list.is_empty());

        let mut list = ManuallyDrop::new(list);

        debug_assert!(list.len() > 0);
        debug_assert!(list.capacity() > 0);

        //safety: we know the list isn't empty so the capacity and length are >0, and the pointer is not null
        let len = NonZeroUsize::new_unchecked(list.len());
        let cap = NonZeroUsize::new_unchecked(list.capacity());
        let ptr = NonNull::new_unchecked(list.as_mut_ptr());

        Self {
            len,
            cap,
            ptr,
            _pd: PhantomData,
        }
    }

    ///NB: must not be called with ZSTs
    fn grow_at_least(&mut self, extra: NonZeroUsize) {
        if size_of::<T>() == 0 {
            panic!("unable to increase capacity size for ZST");
        }
        let extra = extra.get();

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

        let min_cap = self
            .cap
            .checked_add(extra)
            .expect("unable to create large enough list for NonEmptyList");
        if fails_constraints(min_cap) {
            panic!("New list is too large to be a proper vec");
        }

        let new_cap = min_cap
            .checked_next_power_of_two()
            .filter(|x| !fails_constraints(*x))
            .unwrap_or(min_cap);

        //new_size is in bytes, so have to multiply by size_of T
        //safety: this program uses the global allocator
        //safety: same layout because we used the old capacity to generate it
        //safety: new size is > 0 because we added some positive number to it, and then probably made it bigger
        //safety: no overflowing isize::MAX
        let new_ptr = NonNull::new(unsafe {
            realloc(
                self.ptr.as_ptr() as *mut u8,
                old_layout,
                new_cap.get() * size_of::<T>(),
            ) as *mut T
        })
        .expect("reallocation of new empty list failed");

        self.ptr = new_ptr;
        self.cap = new_cap;
    }

    pub fn push(&mut self, el: T) {
        if size_of::<T>() == 0 {
            self.len = self.len.checked_add(1).unwrap();
            return;
        }

        if self.len >= self.cap {
            self.grow_at_least(NonZeroUsize::new(1).unwrap());
        }

        debug_assert!(self.len < self.cap);

        //safety: we know we've allocated enough
        unsafe {
            self.ptr.add(self.len.get()).write(el);
        }

        self.len = self.len.checked_add(1).unwrap();
    }

    pub fn len(&self) -> usize {
        self.len.get()
    }

    pub fn capacity(&self) -> usize {
        self.cap.get()
    }

    pub fn remove(&mut self, index: usize) -> T {
        let current_len = self.len.get();
        if index >= current_len {
            panic!("tried to remove index out of bounds");
        }
        if current_len <= 1 {
            panic!("attempted remove which would make the list empty");
        }

        //safety: checked for index vs current len so we know we have memory at that location
        let currently_at_that_position = unsafe { self.ptr.add(index).read() };

        if index != current_len - 1 {
            //safety: index+1 < current_len, so we know the memory is initialised
            let src = unsafe { self.ptr.add(index + 1) };
            //safety: src is fine so dst is also fine
            let dst = unsafe { self.ptr.add(index) };
            let count = current_len - index - 1;

            debug_assert!(src.is_aligned());
            debug_assert!(dst.is_aligned());

            //safety: both ptrs are valid and aligned
            unsafe {
                src.copy_to(dst, count);
            }
        }

        self.len = NonZeroUsize::try_from(current_len - 1).unwrap();

        currently_at_that_position
    }

    pub fn swap_remove(&mut self, index: usize) -> T {
        let current_len = self.len.get();
        if index >= current_len {
            panic!("tried to remove index out of bounds");
        }
        if current_len <= 1 {
            panic!("attempted remove which would make the list empty");
        }

        //safety: checked vs current len so fine
        let index_ptr = unsafe { self.ptr.add(index) };
        debug_assert!(index_ptr.is_aligned());
        //safety: valid & aligned from above
        let was_at_index = unsafe { index_ptr.read() };

        if index != current_len - 1 {
            //safety: current_len - 1 is the last valid ptr
            let src = unsafe { self.ptr.add(current_len - 1) };
            let dst = index_ptr;

            debug_assert!(src.is_aligned());
            debug_assert!(dst.is_aligned());

            unsafe {
                //safety: both are aligned & valid
                dst.write(src.read());
            }
        }

        self.len = NonZeroUsize::new(current_len - 1).unwrap();
        was_at_index
    }

    pub fn extend(&mut self, iter: impl IntoIterator<Item = T>) {
        let iter = iter.into_iter();

        if size_of::<T>() == 0 {
            let mut count = 0;
            iter.for_each(|_| count += 1);
            self.len = self
                .len
                .checked_add(count)
                .expect("unable to create space for additional ZSTs");
            return;
        }

        let (min, max) = iter.size_hint();
        let (min, max) = (NonZeroUsize::new(min), max.and_then(NonZeroUsize::new));
        match max {
            Some(max) => self.grow_at_least(max),
            None => {
                if let Some(min) = min {
                    self.grow_at_least(min);
                }
            }
        }

        for el in iter {
            //love unreliable size hints
            if self.len == self.cap {
                self.grow_at_least(NonZeroUsize::new(1).unwrap());
            }

            debug_assert!(self.len < self.cap);

            unsafe {
                //safety: just made sure we had space for it, so valid
                let dst = self.ptr.add(self.len.get());

                debug_assert!(dst.is_aligned());

                //safety: dst is valid and aligned
                dst.write(el);
            }

            self.len = self.len.checked_add(1).unwrap();
        }
    }

    pub fn iter(&self) -> slice::Iter<T> {
        self.as_ref().iter()
    }

    pub fn iter_mut(&mut self) -> slice::IterMut<T> {
        self.as_mut().iter_mut()
    }

    ///NB: returns an option because if the retain keeps no elements, then it wouldn't be valid
    pub fn retain<F>(self, f: F) -> Option<Self>
    where
        F: FnMut(&mut T) -> bool,
    {
        let mut v: Vec<T> = self.into();
        v.retain_mut(f);
        Self::new(v)
    }

    pub fn first(&self) -> &T {
        debug_assert!(self.ptr.is_aligned());

        unsafe {
            //safety: we always know that the first element is valid
            //safety: and aligned/nonnull
            //safety: we follow aliasing rules because of the reference to self
            self.ptr.as_ref()
        }
    }

    pub fn first_mut(&mut self) -> &mut T {
        debug_assert!(self.ptr.is_aligned());

        unsafe {
            //safety: we always know that the first element is valid
            //safety: and aligned/nonnull
            //safety: we follow aliasing rules because of the reference to self
            self.ptr.as_mut()
        }
    }

    pub fn last(&self) -> &T {
        debug_assert!(self.ptr.is_aligned());

        unsafe {
            //safety: we know that self.len() >= 1, so it can't underflow and it also has to be valid
            //safety: and aligned/nonnull
            //safety: we follow aliasing rules because of the reference to self
            self.ptr.add(self.len() - 1).as_ref()
        }
    }

    pub fn last_mut(&mut self) -> &mut T {
        debug_assert!(self.ptr.is_aligned());

        unsafe {
            //safety: we know that self.len() >= 1, so it can't underflow and it also has to be valid
            //safety: and aligned/nonnull
            //safety: we follow aliasing rules because of the reference to self
            self.ptr.add(self.len() - 1).as_mut()
        }
    }
}

impl<T> From<NonEmptyList<T>> for Vec<T> {
    fn from(value: NonEmptyList<T>) -> Self {
        debug_assert!(value.len() <= value.capacity());
        debug_assert!((value.capacity() * size_of::<T>()) <= (isize::MAX as usize));

        let md = ManuallyDrop::new(value);
        //safety: we either got it from the global alloc, or a vec
        //safety: T has same alignment as ptr
        //safety: T * capacity is the same as the allocation
        //safety: len <= cap
        //safety: up to len is init
        //safety: initally allocated with cap
        //safety: alloc-ed size is not > isize::MAX
        unsafe { Vec::from_raw_parts(md.ptr.as_ptr(), md.len.get(), md.cap.get()) }
    }
}

impl<T> Drop for NonEmptyList<T> {
    fn drop(&mut self) {
        unsafe {
            //drop all individual elements
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(
                self.ptr.as_ptr(),
                self.len.get(),
            ));

            if size_of::<T>() != 0 {
                let layout = Layout::array::<T>(self.cap.get())
                    .expect("used this alloc to get the allocation");
                dealloc(self.ptr.as_ptr() as *mut u8, layout);
            }
        }
    }
}

impl<T> AsRef<[T]> for NonEmptyList<T> {
    fn as_ref(&self) -> &[T] {
        unsafe { slice::from_raw_parts(self.ptr.as_ptr() as *const _, self.len.get()) }
    }
}
impl<T> AsMut<[T]> for NonEmptyList<T> {
    fn as_mut(&mut self) -> &mut [T] {
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len.get()) }
    }
}

impl<T: Clone> Clone for NonEmptyList<T> {
    fn clone(&self) -> Self {
        if size_of::<T>() == 0 {
            return Self {
                ptr: NonNull::dangling(),
                len: self.len,
                cap: NonZeroUsize::new(usize::MAX).unwrap(),
                _pd: PhantomData,
            };
        }

        let layout = Layout::array::<T>(self.cap.get())
            .expect("unable to create layout for cloning NonEmptyList");
        let ptr = unsafe {
            NonNull::new(alloc(layout) as *mut T).expect("unable to allocate for new NonEmptyList")
        };

        if needs_drop::<T>() {
            for i in 0..self.len.get() {
                //safety: we know everything up to len is alloc-ed
                unsafe {
                    let this_idx = self.ptr.add(i).as_ref();
                    ptr.add(i).write(this_idx.clone());
                }
            }
        } else {
            unsafe {
                self.ptr.copy_to_nonoverlapping(ptr, self.len.get());
            }
        }

        Self {
            ptr,
            len: self.len,
            cap: self.cap,
            _pd: PhantomData,
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

impl<T> Index<usize> for NonEmptyList<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        if index >= self.len() {
            panic!("attempted index out of bounds");
        }

        unsafe { self.ptr.add(index).as_ref() }
    }
}

impl<T> IndexMut<usize> for NonEmptyList<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        if index >= self.len() {
            panic!("attempted index out of bounds");
        }

        unsafe { self.ptr.add(index).as_mut() }
    }
}

#[derive(Default, Clone, Debug)]
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

impl<T> Deref for NonEmptyListBuilder<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T> DerefMut for NonEmptyListBuilder<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> AsRef<[T]> for NonEmptyListBuilder<T> {
    fn as_ref(&self) -> &[T] {
        self.0.as_ref()
    }
}
impl<T> AsMut<[T]> for NonEmptyListBuilder<T> {
    fn as_mut(&mut self) -> &mut [T] {
        self.0.as_mut()
    }
}

//gotta love some chatgpt tests :))))))
#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

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

        list.grow_at_least(NonZeroUsize::new(usize::MAX).unwrap()); // Should panic due to memory constraints
    }

    #[test]
    #[cfg(not(miri))] //takes too long
    fn test_large_extend() {
        let vec = vec![1];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let large_iter = (2..1_000_000).collect::<Vec<_>>();
        list.extend(large_iter);

        assert_eq!(list.len.get(), 1_000_000 - 1);
        assert_eq!(list.as_ref()[0], 1);
        assert_eq!(list.as_ref()[1], 2);
    }

    #[test]
    fn test_non_empty_list_into_iter() {
        let vec = vec![1, 2, 3];
        let list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let collected: Vec<_> = list.into_iter().collect();
        assert_eq!(collected, vec![1, 2, 3]);
    }

    #[test]
    fn test_non_empty_list_push_after_clone() {
        let vec = vec![1, 2];
        let original_list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let mut cloned_list = original_list.clone();
        cloned_list.push(3);

        assert_eq!(original_list.as_ref(), &[1, 2]);
        assert_eq!(cloned_list.as_ref(), &[1, 2, 3]);
    }

    #[test]
    fn test_extend_empty_iterator() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list.extend(Vec::<i32>::new());

        assert_eq!(list.as_ref(), &[1, 2, 3]);
    }

    #[test]
    fn test_as_mut_modifies_elements() {
        let vec = vec![10, 20, 30];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let slice = list.as_mut();
        for element in slice.iter_mut() {
            *element *= 2;
        }

        assert_eq!(list.as_ref(), &[20, 40, 60]);
    }

    #[test]
    fn test_drop_releases_memory() {
        let vec = vec![1, 2, 3];
        let list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        // Explicit drop to ensure memory is released
        drop(list);
        // Since there's no memory tracking here, just ensure no UB or double-free happens.
    }

    #[test]
    fn test_large_push_sequence() {
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec![1]) };

        for i in 2..=1000 {
            list.push(i);
        }

        assert_eq!(list.len.get(), 1000);
        assert_eq!(list.as_ref()[0], 1);
        assert_eq!(list.as_ref()[999], 1000);
    }

    #[test]
    fn test_debug_formatting_with_empty_extend() {
        let mut list = NonEmptyList::new(vec![1, 2, 3]).unwrap();

        list.extend(vec![]); // Extend with empty iterator
        assert_eq!(format!("{:?}", list), "[1, 2, 3]");
    }

    #[test]
    fn test_conversion_to_vec() {
        let vec = vec![1, 2, 3];
        let list = NonEmptyList::new(vec.clone()).unwrap();

        let result_vec: Vec<_> = list.into();
        assert_eq!(result_vec, vec);
    }

    #[test]
    fn test_empty_builder_rejection() {
        let builder = NonEmptyListBuilder::<i32>(vec![]);
        let result = NonEmptyList::try_from(builder);

        assert!(result.is_err());
    }

    #[test]
    #[cfg(not(miri))] //takes too long
    fn test_non_empty_list_large_extend_with_iter() {
        let mut list = NonEmptyList::new(vec![0]).unwrap();

        let range_iter = 1..=1_000_000;
        list.extend(range_iter);

        assert_eq!(list.as_ref()[0], 0);
        assert_eq!(list.as_ref()[1], 1);
        assert_eq!(list.len.get(), 1_000_001);
    }

    #[test]
    fn test_remove_middle_element() {
        let vec = vec![1, 2, 3, 4, 5];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let removed = list.remove(2); // Remove element at index 2
        assert_eq!(removed, 3);
        assert_eq!(list.as_ref(), &[1, 2, 4, 5]);
    }

    #[test]
    fn test_remove_first_element() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let removed = list.remove(0); // Remove first element
        assert_eq!(removed, 1);
        assert_eq!(list.as_ref(), &[2, 3]);
    }

    #[test]
    fn test_remove_last_element() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let removed = list.remove(2); // Remove last element
        assert_eq!(removed, 3);
        assert_eq!(list.as_ref(), &[1, 2]);
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn test_remove_out_of_bounds() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list.remove(3); // Attempt to remove out-of-bounds index
    }

    #[test]
    fn test_swap_remove_middle_element() {
        let vec = vec![1, 2, 3, 4, 5];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let removed = list.swap_remove(2); // Swap and remove element at index 2
        assert_eq!(removed, 3);
        assert_eq!(list.as_ref(), &[1, 2, 5, 4]); // Last element swapped into position 2
    }

    #[test]
    fn test_swap_remove_first_element() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let removed = list.swap_remove(0); // Swap and remove first element
        assert_eq!(removed, 1);
        assert_eq!(list.as_ref(), &[3, 2]); // Last element swapped into position 0
    }

    #[test]
    fn test_swap_remove_last_element() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let removed = list.swap_remove(2); // Swap and remove last element
        assert_eq!(removed, 3);
        assert_eq!(list.as_ref(), &[1, 2]); // List remains unchanged except for the removal
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn test_swap_remove_out_of_bounds() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list.swap_remove(3); // Attempt to swap-remove out-of-bounds index
    }

    #[test]
    #[should_panic(expected = "attempted remove which would make the list empty")]
    fn test_remove_last_element_invalid() {
        let vec = vec![1];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list.remove(0); // Attempt to remove the last remaining element
    }

    #[test]
    #[should_panic(expected = "attempted remove which would make the list empty")]
    fn test_swap_remove_last_element_invalid() {
        let vec = vec![1];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list.swap_remove(0); // Attempt to swap-remove the last remaining element
    }

    #[test]
    fn test_remove_and_push_back() {
        let vec = vec![1, 2, 3, 4];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let removed = list.remove(1); // Remove element at index 1
        assert_eq!(removed, 2);
        assert_eq!(list.as_ref(), &[1, 3, 4]);

        list.push(5);
        assert_eq!(list.as_ref(), &[1, 3, 4, 5]);
    }

    #[test]
    fn test_swap_remove_and_push_back() {
        let vec = vec![1, 2, 3, 4];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let removed = list.swap_remove(1); // Swap and remove element at index 1
        assert_eq!(removed, 2);
        assert_eq!(list.as_ref(), &[1, 4, 3]);

        list.push(5);
        assert_eq!(list.as_ref(), &[1, 4, 3, 5]);
    }

    #[test]
    fn test_index_access() {
        let vec = vec![10, 20, 30];
        let list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        assert_eq!(list[0], 10);
        assert_eq!(list[1], 20);
        assert_eq!(list[2], 30);
    }

    #[test]
    fn test_index_mut_access() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list[1] = 42; // Modify the second element
        assert_eq!(list.as_ref(), &[1, 42, 3]);
    }

    #[test]
    #[should_panic(expected = "attempted index out of bounds")]
    fn test_index_out_of_bounds() {
        let vec = vec![1, 2, 3];
        let list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let _ = list[3]; // Attempt to access out-of-bounds index
    }

    #[test]
    #[should_panic(expected = "attempted index out of bounds")]
    fn test_index_mut_out_of_bounds() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list[3] = 42; // Attempt to modify out-of-bounds index
    }

    #[test]
    fn test_index_with_large_list() {
        let vec = (0..10_000).collect::<Vec<_>>();
        let list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        assert_eq!(list[9999], 9999); // Access last element
        assert_eq!(list[5000], 5000); // Access a middle element
    }

    #[test]
    fn test_index_mut_with_large_list() {
        let vec = (0..10_000).collect::<Vec<_>>();
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        list[9999] = 42; // Modify last element
        list[5000] = 84; // Modify a middle element

        assert_eq!(list[9999], 42);
        assert_eq!(list[5000], 84);
    }

    #[test]
    fn test_index_access_chained() {
        let vec = vec![10, 20, 30];
        let list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        assert_eq!(list[0] + list[1], 30); // Add elements at indices 0 and 1
    }

    #[test]
    fn test_index_mut_with_reassignment() {
        let vec = vec![1, 2, 3];
        let mut list = unsafe { NonEmptyList::from_non_empty_vec(vec) };

        let elem = &mut list[1];
        *elem += 10; // Modify element in-place

        assert_eq!(list.as_ref(), &[1, 12, 3]);
    }

    #[test]
    fn test_new_with_heap_allocated_structures() {
        let vec = vec![Rc::new(1), Rc::new(2), Rc::new(3)];
        let list = NonEmptyList::new(vec).unwrap();

        // Verify all elements are accessible
        assert_eq!(*list[0], 1);
        assert_eq!(*list[1], 2);
        assert_eq!(*list[2], 3);

        // Dropping the list should release all references
        let element = list.into_iter().next().unwrap();
        assert_eq!(Rc::strong_count(&element), 1);
    }

    #[test]
    fn test_single_element_with_heap_allocated_structure() {
        let element = Rc::new(42);
        let list = NonEmptyList::single_element(element.clone());

        // Verify the element is present and reference count is correct
        assert_eq!(*list[0], 42);
        assert_eq!(Rc::strong_count(&element), 2);

        // Dropping the list should decrease the reference count
        drop(list);
        assert_eq!(Rc::strong_count(&element), 1);
    }

    #[test]
    fn test_clone_with_heap_allocated_structures() {
        let vec = vec![Rc::new(1), Rc::new(2)];
        let list = NonEmptyList::new(vec).unwrap();
        let cloned_list = list.clone();

        // Verify both lists contain the same elements
        assert_eq!(*list[0], 1);
        assert_eq!(*list[1], 2);
        assert_eq!(*cloned_list[0], 1);
        assert_eq!(*cloned_list[1], 2);

        // Verify reference counts
        assert_eq!(Rc::strong_count(&list[0]), 2);
        assert_eq!(Rc::strong_count(&list[1]), 2);

        // Dropping one list should not affect the other
        drop(list);
        assert_eq!(Rc::strong_count(&cloned_list[0]), 1);
        assert_eq!(Rc::strong_count(&cloned_list[1]), 1);
    }

    #[derive(Clone)]
    struct DropNotifier(Rc<RefCell<usize>>);
    impl Drop for DropNotifier {
        fn drop(&mut self) {
            *self.0.borrow_mut() += 1;
        }
    }

    #[test]
    fn test_drop_behavior_with_refcell() {
        let drop_count = Rc::new(RefCell::new(0));

        let vec = vec![
            DropNotifier(drop_count.clone()),
            DropNotifier(drop_count.clone()),
            DropNotifier(drop_count.clone()),
        ];

        let list = NonEmptyList::new(vec).unwrap();
        assert_eq!(*drop_count.borrow(), 0); // No elements dropped yet

        drop(list);
        assert_eq!(*drop_count.borrow(), 3); // All elements dropped
    }

    #[test]
    fn test_clone_behavior_with_refcell() {
        let drop_count = Rc::new(RefCell::new(0));

        let vec = vec![
            DropNotifier(drop_count.clone()),
            DropNotifier(drop_count.clone()),
        ];

        let list = NonEmptyList::new(vec).unwrap();
        let cloned_list = list.clone();

        assert_eq!(*drop_count.borrow(), 0); // No elements dropped yet

        drop(list);
        assert_eq!(*drop_count.borrow(), 2); // Cloned list still holds references

        drop(cloned_list);
        assert_eq!(*drop_count.borrow(), 4); // All elements dropped
    }

    #[derive(Debug, PartialEq, Eq, Clone)]
    struct ZST;

    #[test]
    fn test_new_with_zst() {
        let vec = vec![ZST, ZST, ZST];
        let list = NonEmptyList::new(vec).unwrap();

        // Check the length and elements
        assert_eq!(list.len(), 3);
        assert_eq!(list.as_ref(), &[ZST, ZST, ZST]);
    }

    #[test]
    fn test_push_with_zst() {
        let mut list = NonEmptyList::single_element(ZST);

        // Push more ZST elements
        list.push(ZST);
        list.push(ZST);

        assert_eq!(list.len(), 3);
        assert_eq!(list.as_ref(), &[ZST, ZST, ZST]);
    }

    #[test]
    fn test_extend_with_zst() {
        let mut list = NonEmptyList::single_element(ZST);

        // Extend with more ZST elements
        list.extend(vec![ZST, ZST, ZST]);

        assert_eq!(list.len(), 4);
        assert_eq!(list.as_ref(), &[ZST, ZST, ZST, ZST]);
    }

    #[test]
    fn test_remove_with_zst() {
        let vec = vec![ZST, ZST, ZST];
        let mut list = NonEmptyList::new(vec).unwrap();

        // Remove an element
        list.remove(1);

        assert_eq!(list.len(), 2);
        assert_eq!(list.as_ref(), &[ZST, ZST]);
    }

    #[test]
    fn test_swap_remove_with_zst() {
        let vec = vec![ZST, ZST, ZST];
        let mut list = NonEmptyList::new(vec).unwrap();

        // Swap-remove an element
        list.swap_remove(0);

        assert_eq!(list.len(), 2);
        assert_eq!(list.as_ref(), &[ZST, ZST]);
    }

    #[test]
    fn test_clone_with_zst() {
        let vec = vec![ZST, ZST, ZST];
        let list = NonEmptyList::new(vec).unwrap();

        let cloned_list = list.clone();

        // Check both lists contain the same elements
        assert_eq!(list.len(), 3);
        assert_eq!(cloned_list.len(), 3);
        assert_eq!(list.as_ref(), cloned_list.as_ref());
    }

    #[test]
    fn test_retain_mut_with_zst() {
        let vec = vec![ZST, ZST, ZST];
        let mut list = NonEmptyList::new(vec).unwrap();

        // Retain only the first element (dummy condition for ZSTs)
        let mut count = 0;
        list = list
            .retain(|_| {
                count += 1;
                count == 1
            })
            .unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list.as_ref(), &[ZST]);
    }

    #[test]
    fn test_index_and_index_mut_with_zst() {
        let vec = vec![ZST, ZST, ZST];
        let mut list = NonEmptyList::new(vec).unwrap();

        // Access elements
        assert_eq!(list[0], ZST);
        assert_eq!(list[1], ZST);

        // Mutate elements (no-op for ZSTs)
        list[0] = ZST;
        list[1] = ZST;

        assert_eq!(list[0], ZST);
        assert_eq!(list[1], ZST);
    }
}
