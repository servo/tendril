// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Provides an unsafe owned buffer type, used in implementing `Tendril`.

#![allow(dead_code, unused_imports)]

use std::{raw, mem, str, ptr, cmp, intrinsics, u32};
use std::rt::heap;
use std::marker::PhantomData;
use std::cell::Cell;
use std::ops::Deref;
use alloc::oom;

use OFLOW;

pub const MIN_CAP: u32 = 16;

// NB: This alignment must be sufficient for H!
pub const MIN_ALIGN: usize = 4;

pub const MAX_LEN: usize = u32::MAX as usize;

/// A buffer points to a header of type `H`, which is followed by `MIN_CAP` or more
/// bytes of storage.
#[repr(packed)]
pub struct Buf32<H> {
    pub ptr: *mut H,
    pub len: u32,
    pub cap: u32,
}

#[inline(always)]
fn add_header<H>(x: u32) -> usize {
    (x as usize).checked_add(mem::size_of::<H>())
        .expect(OFLOW)
}

#[inline(always)]
fn full_cap<H>(size: usize) -> u32 {
    cmp::min(u32::MAX as usize,
        heap::usable_size(size, MIN_ALIGN)
            .checked_sub(mem::size_of::<H>())
            .expect(OFLOW)) as u32
}

impl<H> Buf32<H> {
    #[inline]
    pub unsafe fn with_capacity(mut cap: u32, h: H) -> Buf32<H> {
        if cap < MIN_CAP {
            cap = MIN_CAP;
        }

        let alloc_size = add_header::<H>(cap);
        let ptr = heap::allocate(alloc_size, MIN_ALIGN);
        if ptr.is_null() {
            ::alloc::oom();
        }

        let ptr = ptr as *mut H;
        ptr::write(ptr, h);

        Buf32 {
            ptr: ptr,
            len: 0,
            cap: full_cap::<H>(alloc_size),
        }
    }

    #[inline]
    pub unsafe fn new(h: H) -> Buf32<H> {
        Buf32::with_capacity(MIN_CAP, h)
    }

    #[inline]
    pub unsafe fn destroy(self) {
        let alloc_size = add_header::<H>(self.cap);
        heap::deallocate(self.ptr as *mut u8, alloc_size, MIN_ALIGN);
    }

    #[inline(always)]
    pub unsafe fn header(&self) -> &H {
        mem::transmute(self.ptr)
    }

    #[inline(always)]
    pub unsafe fn data_ptr(&self) -> *mut u8 {
        (self.ptr as *mut u8).offset(mem::size_of::<H>() as isize)
    }

    #[inline(always)]
    pub unsafe fn data_raw(&self) -> raw::Slice<u8> {
        raw::Slice {
            data: self.data_ptr(),
            len: self.len as usize,
        }
    }

    #[inline(always)]
    pub unsafe fn buffer_raw(&self) -> raw::Slice<u8> {
        raw::Slice {
            data: self.data_ptr(),
            len: self.cap as usize,
        }
    }

    #[inline(always)]
    pub unsafe fn data(&self) -> &[u8] {
        mem::transmute(self.data_raw())
    }

    #[inline(always)]
    pub unsafe fn data_mut(&mut self) -> &mut [u8] {
        mem::transmute(self.data_raw())
    }

    #[inline(always)]
    pub unsafe fn buffer(&self) -> &[u8] {
        mem::transmute(self.buffer_raw())
    }

    #[inline(always)]
    pub unsafe fn buffer_mut(&mut self) -> &mut [u8] {
        mem::transmute(self.buffer_raw())
    }

    /// Grow the capacity to at least `new_cap`.
    ///
    /// This will panic if the capacity calculation overflows `u32`.
    #[inline]
    pub unsafe fn grow(&mut self, new_cap: u32) {
        if new_cap <= self.cap {
            return;
        }

        let new_cap = new_cap.checked_next_power_of_two().expect(OFLOW);
        let alloc_size = add_header::<H>(new_cap);
        let ptr = heap::reallocate(self.ptr as *mut u8,
                                   add_header::<H>(new_cap),
                                   alloc_size,
                                   MIN_ALIGN);
        if ptr.is_null() {
            ::alloc::oom();
        }

        self.ptr = ptr as *mut H;
        self.cap = full_cap::<H>(alloc_size);
    }
}

#[cfg(test)]
mod test {
    use super::Buf32;
    use std::slice::bytes;

    #[test]
    fn smoke_test() {
        unsafe {
            let mut b = Buf32::new(());
            assert_eq!(&[], b.data());

            b.grow(5);
            bytes::copy_memory(b.buffer_mut(), b"Hello");

            assert_eq!(&[], b.data());
            b.len = 5;
            assert_eq!(b"Hello", b.data());

            b.grow(1337);
            assert!(b.cap >= 1337);
            assert_eq!(b"Hello", b.data());

            b.destroy();
        }
    }
}
