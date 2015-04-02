// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::{slice, intrinsics};
use std::raw::{self, Repr};

#[inline(always)]
pub unsafe fn unsafe_slice<'a>(buf: &'a [u8], start: usize, new_len: usize) -> &'a [u8] {
    let raw::Slice { data, len } = buf.repr();
    debug_assert!(start <= len);
    debug_assert!(new_len <= (len - start));
    slice::from_raw_parts(data.offset(start as isize), new_len)
}

#[inline(always)]
pub unsafe fn copy_and_advance(dest: &mut *mut u8, src: &[u8]) {
    intrinsics::copy_nonoverlapping(src.as_ptr(), *dest, src.len());
    *dest = dest.offset(src.len() as isize)
}
