//! The C forms of the values that cross.
//!
//! A number, `bool` or `char` crosses as a C scalar of its width. Everything else is
//! one of the two structs here, nested as the type is nested. The Jade compiler
//! builds and reads the same structs from the type alone (`compiler/src/backend/
//! native.rs`), so these layouts are part of the description's format: a change here
//! is a change to `meta::FORMAT`.
//!
//! Every C form is plain data, so a zeroed one is valid: integers, floats and raw
//! pointers, in `#[repr(C)]` structs.

/// The C form of a `str`, `bytes` or array: a pointer to `len` elements, and the
/// capacity of the allocation behind it.
///
/// Going into Rust, the memory is the Jade program's, lent for the call, and `cap` is
/// 0. Coming back, it is a Rust `Vec`'s, taken apart with its capacity so the
/// function's `__free` companion can put it back together and drop it.
#[repr(C)]
pub struct CVec<T> {
    pub ptr: *mut T,
    pub len: usize,
    pub cap: usize,
}

// Written out rather than derived: a derive would require `T: Copy`, and the struct
// only holds a pointer to `T`.
impl<T> Clone for CVec<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for CVec<T> {}

impl<T> CVec<T> {
    /// Purpose: This function hands a `Vec` over in its C form.
    ///
    /// Inputs:
    /// - `v`: the vector, which the C form now owns.
    ///
    /// Outputs: the pointer, length and capacity.
    pub fn from_vec(v: Vec<T>) -> Self {
        let mut v = std::mem::ManuallyDrop::new(v);
        CVec { ptr: v.as_mut_ptr(), len: v.len(), cap: v.capacity() }
    }

    /// Purpose: This function takes back a `Vec` that [`CVec::from_vec`] handed over.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the vector.
    ///
    /// # Safety
    ///
    /// `self` must have come from `from_vec`, and not been taken back already.
    pub unsafe fn into_vec(self) -> Vec<T> {
        Vec::from_raw_parts(self.ptr, self.len, self.cap)
    }

    /// Purpose: This function lends the elements as a slice.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the elements. An empty one may carry any pointer, so it is not read.
    ///
    /// # Safety
    ///
    /// `ptr` must point at `len` initialized elements for as long as the slice lives.
    pub unsafe fn as_slice<'a>(&self) -> &'a [T] {
        if self.len == 0 {
            return &[];
        }
        std::slice::from_raw_parts(self.ptr, self.len)
    }
}

/// The C form of an optional: whether there is a value, then the value's C form,
/// zeroed when there is none.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct COpt<T: Copy> {
    pub some: u8,
    pub value: T,
}
