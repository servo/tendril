// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::{raw, ptr, mem, intrinsics, hash, str, io};
use std::marker::PhantomData;
use std::cell::Cell;
use std::ops::Deref;
use std::iter::IntoIterator;
use std::default::Default;
use std::cmp::Ordering;
use std::fmt as strfmt;

use core::nonzero::NonZero;

use buf32::{self, Buf32};
use fmt::{self, Slice};
use fmt::imp::Fixup;
use util::{unsafe_slice, copy_and_advance};
use OFLOW;

const MAX_INLINE_LEN: usize = 8;
const MAX_INLINE_TAG: usize = 0xF;
const EMPTY_TAG: usize = 0xF;

struct Header {
    refcount: Cell<usize>,
    cap: u32,
}

impl Header {
    #[inline(always)]
    unsafe fn new() -> Header {
        Header {
            refcount: Cell::new(1),
            cap: mem::uninitialized(),
        }
    }
}

#[derive(Copy, Clone, Hash, Debug, PartialEq, Eq)]
pub enum SubtendrilError {
    OutOfBounds,
    ValidationFailed,
}

#[unsafe_no_drop_flag]
#[repr(packed)]
pub struct Tendril<F> {
    ptr: Cell<NonZero<usize>>,
    len: u32,
    aux: Cell<u32>,
    marker: PhantomData<*mut F>,
}

/// `Tendril` for storing native Rust strings.
pub type StrTendril = Tendril<fmt::UTF8>;

/// `Tendril` for storing binary data.
pub type ByteTendril = Tendril<fmt::Bytes>;

impl<F> Clone for Tendril<F>
    where F: fmt::Format,
{
    #[inline]
    fn clone(&self) -> Tendril<F> {
        unsafe {
            if *self.ptr.get() > MAX_INLINE_TAG {
                self.make_buf_shared();
                self.incref();
            }

            ptr::read(self)
        }
    }
}

#[unsafe_destructor]
impl<F> Drop for Tendril<F>
    where F: fmt::Format,
{
    #[inline]
    fn drop(&mut self) {
        unsafe {
            if *self.ptr.get() <= MAX_INLINE_TAG {
                return;
            }

            let (buf, shared, _) = self.assume_buf();
            if shared {
                let header = self.header();
                let refcount = (*header).refcount.get() - 1;
                if refcount == 0 {
                    buf.destroy();
                } else {
                    (*header).refcount.set(refcount);
                }
            } else {
                buf.destroy();
            }
        }
    }
}

// impl FromIterator<char> for Tendril<fmt::UTF8> { }
// impl FromIterator<u8> for Tendril<fmt::Bytes> { }

impl<F> Deref for Tendril<F>
    where F: fmt::SliceFormat,
{
    type Target = F::Slice;

    #[inline]
    fn deref(&self) -> &F::Slice {
        unsafe {
            F::Slice::from_bytes(self.as_byte_slice())
        }
    }
}

impl<'a, F> Extend<&'a Tendril<F>> for Tendril<F>
    where F: fmt::Format + 'a,
{
    #[inline]
    fn extend<I>(&mut self, iterable: I)
        where I: IntoIterator<Item = &'a Tendril<F>>,
    {
        let iterator = iterable.into_iter();
        for t in iterator {
            self.push_tendril(t);
        }
    }
}

impl<F> PartialEq for Tendril<F>
    where F: fmt::Format,
{
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_byte_slice() == other.as_byte_slice()
    }

    #[inline]
    fn ne(&self, other: &Self) -> bool {
        self.as_byte_slice() != other.as_byte_slice()
    }
}

impl<F> Eq for Tendril<F>
    where F: fmt::Format,
{ }

impl<F> PartialOrd for Tendril<F>
    where F: fmt::SliceFormat,
          <F as fmt::SliceFormat>::Slice: PartialOrd,
{
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        PartialOrd::partial_cmp(&**self, &**other)
    }
}

impl<F> Ord for Tendril<F>
    where F: fmt::SliceFormat,
          <F as fmt::SliceFormat>::Slice: Ord,
{
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        Ord::cmp(&**self, &**other)
    }
}

impl<F> Default for Tendril<F>
    where F: fmt::Format,
{
    #[inline(always)]
    fn default() -> Tendril<F> {
        Tendril::new()
    }
}

impl<F> strfmt::Debug for Tendril<F>
    where F: fmt::SliceFormat + Default + strfmt::Debug,
          <F as fmt::SliceFormat>::Slice: strfmt::Debug,
{
    #[inline]
    fn fmt(&self, f: &mut strfmt::Formatter) -> strfmt::Result {
        let kind = match *self.ptr.get() {
            p if p <= MAX_INLINE_LEN => "inline",
            p if p & 1 == 1 => "shared",
            _ => "owned",
        };

        try!(write!(f, "Tendril<{:?}>({}: ", <F as Default>::default(), kind));
        try!(<<F as fmt::SliceFormat>::Slice as strfmt::Debug>::fmt(&**self, f));
        write!(f, ")")
    }
}

impl<F> hash::Hash for Tendril<F>
    where F: fmt::Format,
{
    #[inline]
    fn hash<H: hash::Hasher>(&self, hasher: &mut H) {
        self.as_byte_slice().hash(hasher)
    }
}

impl<F> Tendril<F>
    where F: fmt::Format,
{
    /// Create a new, empty `Tendril` in any format.
    #[inline(always)]
    pub fn new() -> Tendril<F> {
        unsafe {
            Tendril::inline(&[])
        }
    }

    /// Get the length of the `Tendril`.
    ///
    /// This is named not to conflict with `len()` on the underlying
    /// slice, if any.
    #[inline(always)]
    pub fn len32(&self) -> u32 {
        match *self.ptr.get() {
            EMPTY_TAG => 0,
            n if n <= MAX_INLINE_LEN => n as u32,
            _ => self.len,
        }
    }

    /// Is the backing buffer shared?
    #[inline(always)]
    pub fn is_shared(&self) -> bool {
        let n = *self.ptr.get();

        (n > MAX_INLINE_LEN) && ((n & 1) == 1)
    }

    /// Truncate to length 0 without discarding any owned storage.
    #[inline]
    pub fn clear(&mut self) {
        if *self.ptr.get() <= MAX_INLINE_LEN {
            self.ptr.set(unsafe { NonZero::new(EMPTY_TAG) });
        } else {
            let (_, shared, _) = unsafe { self.assume_buf() };
            if shared {
                // No need to keep a reference alive for a 0-size slice.
                *self = Tendril::new();
            } else {
                self.len = 0;
            }
        }
    }

    /// Build a `Tendril` by copying a byte slice, if it conforms to the format.
    #[inline]
    pub fn try_from_byte_slice(x: &[u8]) -> Result<Tendril<F>, ()> {
        match F::validate(x) {
            true => Ok(unsafe { Tendril::from_byte_slice_without_validating(x) }),
            false => Err(()),
        }
    }

    /// View as uninterpreted bytes.
    #[inline(always)]
    pub fn as_bytes(&self) -> &Tendril<fmt::Bytes> {
        unsafe { mem::transmute(self) }
    }

    /// Convert into uninterpreted bytes.
    #[inline(always)]
    pub fn into_bytes(self) -> Tendril<fmt::Bytes> {
        unsafe { mem::transmute(self) }
    }

    /// View as a superset format, for free.
    #[inline(always)]
    pub fn as_superset<Super>(&self) -> &Tendril<Super>
        where F: fmt::SubsetOf<Super>,
    {
        unsafe { mem::transmute(self) }
    }

    /// Convert into a superset format, for free.
    #[inline(always)]
    pub fn into_superset<Super>(self) -> Tendril<Super>
        where F: fmt::SubsetOf<Super>,
    {
        unsafe { mem::transmute(self) }
    }

    /// View as a subset format, if the `Tendril` conforms to that subset.
    #[inline]
    pub fn try_as_subset<Sub>(&self) -> Result<&Tendril<Sub>, ()>
        where Sub: fmt::SubsetOf<F>,
    {
        match Sub::revalidate_subset(self.as_byte_slice()) {
            true => Ok(unsafe { mem::transmute(self) }),
            false => Err(()),
        }
    }

    /// Convert into a subset format, if the `Tendril` conforms to that subset.
    #[inline]
    pub fn try_into_subset<Sub>(self) -> Result<Tendril<Sub>, Self>
        where Sub: fmt::SubsetOf<F>,
    {
        match Sub::revalidate_subset(self.as_byte_slice()) {
            true => Ok(unsafe { mem::transmute(self) }),
            false => Err(self),
        }
    }

    /// View as another format, if the `Tendril` conforms to that format.
    #[inline]
    pub fn try_as_other_format<Other>(&self) -> Result<&Tendril<Other>, ()>
        where Other: fmt::Format,
    {
        match Other::validate(self.as_byte_slice()) {
            true => Ok(unsafe { mem::transmute(self) }),
            false => Err(()),
        }
    }

    /// Convert into another format, if the `Tendril` conforms to that format.
    #[inline]
    pub fn try_into_other_format<Other>(self) -> Result<Tendril<Other>, Self>
        where Other: fmt::Format,
    {
        match Other::validate(self.as_byte_slice()) {
            true => Ok(unsafe { mem::transmute(self) }),
            false => Err(self),
        }
    }

    /// Push some bytes onto the end of the `Tendril`, if they conform to the
    /// format.
    #[inline]
    pub fn try_push_bytes(&mut self, buf: &[u8]) -> Result<(), ()> {
        match F::validate(buf) {
            true => unsafe {
                self.push_bytes_without_validating(buf);
                Ok(())
            },
            false => Err(()),
        }
    }

    /// Push another `Tendril` onto the end of this one.
    #[inline]
    pub fn push_tendril(&mut self, other: &Tendril<F>) {
        let new_len = self.len32().checked_add(other.len32()).expect(OFLOW);

        unsafe {
            if (*self.ptr.get() > MAX_INLINE_LEN) && (*other.ptr.get() > MAX_INLINE_LEN) {
                let (self_buf, self_shared, _) = self.assume_buf();
                let (other_buf, other_shared, _) = other.assume_buf();

                if self_shared && other_shared
                    && (self_buf.data_ptr() == other_buf.data_ptr())
                    && (other.aux.get() == self.aux.get() + self.len)
                {
                    self.len = new_len;
                    return;
                }
            }

            self.push_bytes_without_validating(other.as_byte_slice())
        }
    }

    /// Attempt to slice this `Tendril` as a new `Tendril`.
    ///
    /// This will share the buffer when possible. Mutating a shared buffer
    /// will copy the contents.
    ///
    /// The offset and length are in bytes. The function will return
    /// `Err` if these are out of bounds, or if the resulting slice
    /// does not conform to the format.
    #[inline]
    pub fn try_subtendril(&self, offset: u32, length: u32)
        -> Result<Tendril<F>, SubtendrilError>
    {
        let self_len = self.len32();
        if offset > self_len || length > (self_len - offset) {
            return Err(SubtendrilError::OutOfBounds);
        }

        unsafe {
            let byte_slice = unsafe_slice(self.as_byte_slice(),
                offset as usize, length as usize);
            if !F::validate_subseq(byte_slice) {
                return Err(SubtendrilError::ValidationFailed);
            }

            Ok(self.unsafe_subtendril(offset, length))
        }
    }

    /// Slice this `Tendril` as a new `Tendril`.
    ///
    /// Panics on bounds or validity check failure.
    #[inline]
    pub fn subtendril(&self, offset: u32, length: u32) -> Tendril<F> {
        self.try_subtendril(offset, length).unwrap()
    }

    /// Try to drop `n` bytes from the front.
    ///
    /// Returns `Err` if the bytes are not available, or the suffix fails
    /// validation.
    #[inline]
    pub fn try_pop_front(&mut self, n: u32) -> Result<(), SubtendrilError> {
        let old_len = self.len32();
        if n > old_len {
            return Err(SubtendrilError::OutOfBounds);
        }
        let new_len = old_len - n;

        unsafe {
            if !F::validate_suffix(unsafe_slice(self.as_byte_slice(),
                                                n as usize, new_len as usize)) {
                return Err(SubtendrilError::ValidationFailed);
            }

            self.unsafe_pop_front(n);
            Ok(())
        }
    }

    /// Drop `n` bytes from the front.
    ///
    /// Panics if the bytes are not available, or the suffix fails
    /// validation.
    #[inline(always)]
    pub fn pop_front(&mut self, n: u32) {
        self.try_pop_front(n).unwrap()
    }

    /// Drop `n` bytes from the back.
    ///
    /// Returns `Err` if the bytes are not available, or the prefix fails
    /// validation.
    #[inline]
    pub fn try_pop_back(&mut self, n: u32) -> Result<(), SubtendrilError> {
        let old_len = self.len32();
        if n > old_len {
            return Err(SubtendrilError::OutOfBounds);
        }
        let new_len = old_len - n;

        unsafe {
            if !F::validate_prefix(unsafe_slice(self.as_byte_slice(),
                                                0, new_len as usize)) {
                return Err(SubtendrilError::ValidationFailed);
            }

            self.unsafe_pop_back(n);
            Ok(())
        }
    }

    /// Drop `n` bytes from the back.
    ///
    /// Panics if the bytes are not available, or the prefix fails
    /// validation.
    #[inline(always)]
    pub fn pop_back(&mut self, n: u32) {
        self.try_pop_back(n).unwrap()
    }

    /// View as another format, without validating.
    #[inline(always)]
    pub unsafe fn as_other_format_without_validating<Other>(&self) -> &Tendril<Other>
        where Other: fmt::Format,
    {
        mem::transmute(self)
    }

    /// Convert into another format, without validating.
    #[inline(always)]
    pub unsafe fn into_other_format_without_validating<Other>(self) -> Tendril<Other>
        where Other: fmt::Format,
    {
        mem::transmute(self)
    }

    /// Build a `Tendril` by copying a byte slice, without validating.
    #[inline]
    pub unsafe fn from_byte_slice_without_validating(x: &[u8]) -> Tendril<F> {
        assert!(x.len() <= buf32::MAX_LEN);
        if x.len() <= MAX_INLINE_LEN {
            Tendril::inline(x)
        } else {
            Tendril::owned_copy(x)
        }
    }

    /// Push some bytes onto the end of the `Tendril`, without validating.
    #[inline]
    pub unsafe fn push_bytes_without_validating(&mut self, buf: &[u8]) {
        assert!(buf.len() <= buf32::MAX_LEN);

        let Fixup { drop_left, drop_right, insert_len, insert_bytes }
            = F::fixup(self.as_byte_slice(), buf);

        // FIXME: think more about overflow
        let adj_len = self.len32() + insert_len - drop_left;

        let new_len = adj_len.checked_add(buf.len() as u32).expect(OFLOW)
            - drop_right;

        let drop_left = drop_left as usize;
        let drop_right = drop_right as usize;

        if new_len <= MAX_INLINE_LEN as u32 {
            let mut tmp: [u8; MAX_INLINE_LEN] = mem::uninitialized();
            {
                let old = self.as_byte_slice();
                let mut dest = tmp.as_mut_ptr();
                copy_and_advance(&mut dest, unsafe_slice(old, 0, old.len() - drop_left));
                copy_and_advance(&mut dest, unsafe_slice(&insert_bytes, 0, insert_len as usize));
                copy_and_advance(&mut dest, unsafe_slice(buf, drop_right, buf.len() - drop_right));
            }
            *self = Tendril::inline(&tmp[..new_len as usize]);
        } else {
            self.make_owned_with_capacity(new_len);
            let (owned, _, _) = self.assume_buf();
            let mut dest = owned.data_ptr().offset((owned.len as usize - drop_left) as isize);
            copy_and_advance(&mut dest, unsafe_slice(&insert_bytes, 0, insert_len as usize));
            copy_and_advance(&mut dest, unsafe_slice(buf, drop_right, buf.len() - drop_right));
            self.len = new_len;
        }
    }

    /// Slice this `Tendril` as a new `Tendril`.
    ///
    /// Does not check validity or bounds!
    #[inline]
    pub unsafe fn unsafe_subtendril(&self, offset: u32, length: u32) -> Tendril<F> {
        if *self.ptr.get() <= MAX_INLINE_LEN {
            Tendril::inline(unsafe_slice(self.as_byte_slice(),
                offset as usize, length as usize))
        } else {
            self.make_buf_shared();
            self.incref();
            let (buf, _, _) = self.assume_buf();
            Tendril::shared(buf, self.aux.get() + offset, length)
        }
    }

    /// Drop `n` bytes from the front.
    ///
    /// Does not check validity or bounds!
    #[inline]
    pub unsafe fn unsafe_pop_front(&mut self, n: u32) {
        let new_len = self.len32() - n;
        if new_len <= MAX_INLINE_LEN as u32 {
             *self = Tendril::inline(unsafe_slice(self.as_byte_slice(),
                n as usize, new_len as usize));
        } else {
            self.make_buf_shared();
            self.aux.set(self.aux.get() + n);
            self.len -= n;
        }
    }

    /// Drop `n` bytes from the back.
    ///
    /// Does not check validity or bounds!
    #[inline]
    pub unsafe fn unsafe_pop_back(&mut self, n: u32) {
        let new_len = self.len32() - n;
        if new_len <= MAX_INLINE_LEN as u32 {
             *self = Tendril::inline(unsafe_slice(self.as_byte_slice(),
                0, new_len as usize));
        } else {
            self.make_buf_shared();
            self.len -= n;
        }
    }

    #[inline(always)]
    fn as_byte_slice<'a>(&'a self) -> &'a [u8] {
        unsafe {
            match *self.ptr.get() {
                EMPTY_TAG => mem::transmute(raw::Slice {
                    data: ptr::null::<u8>(),
                    len: 0,
                }),
                n if n <= MAX_INLINE_LEN => mem::transmute(raw::Slice {
                    data: &self.len as *const u32 as *const u8,
                    len: n,
                }),
                _ => {
                    let (buf, _, offset) = self.assume_buf();
                    mem::copy_lifetime(self, unsafe_slice(buf.data(),
                        offset as usize, self.len32() as usize))
                }
            }
        }
    }

    #[inline(always)]
    unsafe fn incref(&self) {
        let header = self.header();
        let refcount = (*header).refcount.get().checked_add(1).expect(OFLOW);
        (*header).refcount.set(refcount);
    }

    #[inline(always)]
    unsafe fn make_buf_shared(&self) {
        let p = *self.ptr.get();
        if p & 1 == 0 {
            let header = p as *mut Header;
            (*header).cap = self.aux.get();

            self.ptr.set(NonZero::new(p | 1));
            self.aux.set(0);
        }
    }

    #[inline(always)]
    unsafe fn make_owned_with_capacity(&mut self, cap: u32) {
        let ptr = *self.ptr.get();
        if ptr <= MAX_INLINE_LEN || (ptr & 1) == 1 {
            *self = Tendril::owned_copy(self.as_byte_slice());
        }
        self.assume_buf().0.grow(cap);
    }

    #[inline(always)]
    unsafe fn header(&self) -> *mut Header {
        (*self.ptr.get() & !1) as *mut Header
    }

    #[inline(always)]
    unsafe fn assume_buf(&self) -> (Buf32<Header>, bool, u32) {
        let ptr = self.ptr.get();
        let header = self.header();
        let shared = (*ptr & 1) == 1;
        let (cap, offset) = match shared {
            true => ((*header).cap, self.aux.get()),
            false => (self.aux.get(), 0),
        };

        (Buf32 {
            ptr: header,
            len: offset + self.len32(),
            cap: cap,
        }, shared, offset)
    }

    #[inline(always)]
    unsafe fn inline(x: &[u8]) -> Tendril<F> {
        debug_assert!(x.len() <= MAX_INLINE_LEN);

        let len = x.len();
        let mut t = Tendril {
            ptr: Cell::new(NonZero::new(if len == 0 { EMPTY_TAG } else { len })),
            len: mem::uninitialized(),
            aux: mem::uninitialized(),
            marker: PhantomData,
        };
        intrinsics::copy_nonoverlapping(&mut t.len as *mut u32 as *mut u8,
                                        x.as_ptr(), len);
        t
    }

    #[inline(always)]
    unsafe fn owned(x: Buf32<Header>) -> Tendril<F> {
        Tendril {
            ptr: Cell::new(NonZero::new(x.ptr as usize)),
            len: x.len,
            aux: Cell::new(x.cap),
            marker: PhantomData,
        }
    }

    #[inline]
    unsafe fn owned_copy(x: &[u8]) -> Tendril<F> {
        let len32 = x.len() as u32;
        let mut b = Buf32::with_capacity(len32, Header::new());
        intrinsics::copy_nonoverlapping(b.data_ptr(), x.as_ptr(), x.len());
        b.len = len32;
        Tendril::owned(b)
    }

    #[inline(always)]
    unsafe fn shared(buf: Buf32<Header>, off: u32, len: u32) -> Tendril<F> {
        Tendril {
            ptr: Cell::new(NonZero::new((buf.ptr as usize) | 1)),
            len: len,
            aux: Cell::new(off),
            marker: PhantomData,
        }
    }
}

impl<F> Tendril<F>
    where F: fmt::SliceFormat,
{
    /// Build a `Tendril` by copying a slice.
    #[inline]
    pub fn from_slice(x: &F::Slice) -> Tendril<F> {
        unsafe {
            Tendril::from_byte_slice_without_validating(x.as_bytes())
        }
    }

    /// Push a slice onto the end of the `Tendril`.
    #[inline]
    pub fn push_slice(&mut self, x: &F::Slice) {
        unsafe {
            self.push_bytes_without_validating(x.as_bytes())
        }
    }
}

/// `Tendril`-related methods for Rust slices.
pub trait SliceExt: fmt::Slice {
    /// Make a `Tendril` from this slice.
    #[inline(always)]
    fn to_tendril(&self) -> Tendril<Self::Format> {
        Tendril::from_slice(self)
    }
}

impl SliceExt for str { }
impl SliceExt for [u8] { }

impl io::Write for Tendril<fmt::Bytes> {
    #[inline(always)]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.push_slice(buf);
        Ok(buf.len())
    }

    #[inline(always)]
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.push_slice(buf);
        Ok(())
    }

    #[inline(always)]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl strfmt::Display for Tendril<fmt::UTF8> {
    #[inline(always)]
    fn fmt(&self, f: &mut strfmt::Formatter) -> strfmt::Result {
        <str as strfmt::Display>::fmt(&**self, f)
    }
}

impl str::FromStr for Tendril<fmt::UTF8> {
    type Err = ();

    #[inline(always)]
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(Tendril::from_slice(s))
    }
}

impl strfmt::Write for Tendril<fmt::UTF8> {
    #[inline(always)]
    fn write_str(&mut self, s: &str) -> strfmt::Result {
        self.push_slice(s);
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::{Tendril, ByteTendril, StrTendril, SliceExt};
    use fmt;

    #[test]
    fn smoke_test() {
        assert_eq!("", &*"".to_tendril());
        assert_eq!("abc", &*"abc".to_tendril());
        assert_eq!("Hello, world!", &*"Hello, world!".to_tendril());

        assert_eq!(b"", &*b"".to_tendril());
        assert_eq!(b"abc", &*b"abc".to_tendril());
        assert_eq!(b"Hello, world!", &*b"Hello, world!".to_tendril());
    }

    #[test]
    fn assert_sizes() {
        use std::mem;
        let correct = mem::size_of::<*const ()>() + 8;
        assert_eq!(correct, mem::size_of::<ByteTendril>());
        assert_eq!(correct, mem::size_of::<StrTendril>());

        // Check that the NonZero<T> optimization is working.
        assert_eq!(correct, mem::size_of::<Option<ByteTendril>>());
        assert_eq!(correct, mem::size_of::<Option<StrTendril>>());
    }

    #[test]
    fn validate_utf8() {
        assert!(ByteTendril::try_from_byte_slice(b"\xFF").is_ok());
        assert!(StrTendril::try_from_byte_slice(b"\xFF").is_err());
        assert!(StrTendril::try_from_byte_slice(b"\xEA\x99\xFF").is_err());
        assert!(StrTendril::try_from_byte_slice(b"\xEA\x99").is_err());
        assert!(StrTendril::try_from_byte_slice(b"\xEA\x99\xAE\xEA").is_err());
        assert_eq!("\u{a66e}", &*StrTendril::try_from_byte_slice(b"\xEA\x99\xAE").unwrap());

        let mut t = StrTendril::new();
        assert!(t.try_push_bytes(b"\xEA\x99").is_err());
        assert!(t.try_push_bytes(b"\xAE").is_err());
        assert!(t.try_push_bytes(b"\xEA\x99\xAE").is_ok());
        assert_eq!("\u{a66e}", &*t);
    }

    #[test]
    fn share_and_unshare() {
        let s = b"foobarbaz".to_tendril();
        assert_eq!(b"foobarbaz", &*s);
        assert!(!s.is_shared());

        let mut t = s.clone();
        assert_eq!(s.as_ptr(), t.as_ptr());
        assert!(s.is_shared());
        assert!(t.is_shared());

        t.push_slice(b"quux");
        assert_eq!(b"foobarbaz", &*s);
        assert_eq!(b"foobarbazquux", &*t);
        assert!(s.as_ptr() != t.as_ptr());
        assert!(!t.is_shared());
    }

    #[test]
    fn format_display() {
        assert_eq!("foobar", &*format!("{}", "foobar".to_tendril()));

        let mut s = "foo".to_tendril();
        assert_eq!("foo", &*format!("{}", s));

        let t = s.clone();
        assert_eq!("foo", &*format!("{}", s));
        assert_eq!("foo", &*format!("{}", t));

        s.push_slice("barbaz!");
        assert_eq!("foobarbaz!", &*format!("{}", s));
        assert_eq!("foo", &*format!("{}", t));
    }

    #[test]
    fn format_debug() {
        assert_eq!(r#"Tendril<UTF8>(inline: "foobar")"#,
                   &*format!("{:?}", "foobar".to_tendril()));
        assert_eq!(r#"Tendril<Bytes>(inline: [102, 111, 111, 98, 97, 114])"#,
                   &*format!("{:?}", b"foobar".to_tendril()));

        let t = "anextralongstring".to_tendril();
        assert_eq!(r#"Tendril<UTF8>(owned: "anextralongstring")"#,
                   &*format!("{:?}", t));
        t.clone();
        assert_eq!(r#"Tendril<UTF8>(shared: "anextralongstring")"#,
                   &*format!("{:?}", t));
    }

    #[test]
    fn subtendril() {
        assert_eq!("foo".to_tendril(), "foo-bar".to_tendril().subtendril(0, 3));
        assert_eq!("bar".to_tendril(), "foo-bar".to_tendril().subtendril(4, 3));

        let mut t = "foo-bar".to_tendril();
        t.pop_front(2);
        assert_eq!("o-bar".to_tendril(), t);
        t.pop_back(1);
        assert_eq!("o-ba".to_tendril(), t);

        assert_eq!("foo".to_tendril(),
            "foo-a-longer-string-bar-baz".to_tendril().subtendril(0, 3));
        assert_eq!("oo-a-".to_tendril(),
            "foo-a-longer-string-bar-baz".to_tendril().subtendril(1, 5));
        assert_eq!("bar".to_tendril(),
            "foo-a-longer-string-bar-baz".to_tendril().subtendril(20, 3));

        let mut t = "another rather long string".to_tendril();
        t.pop_front(2);
        assert!(t.starts_with("other rather"));
        t.pop_back(1);
        assert_eq!("other rather long strin".to_tendril(), t);
        assert!(t.is_shared());
    }

    #[test]
    fn subtendril_invalid() {
        assert!("\u{a66e}".to_tendril().try_subtendril(0, 2).is_err());
        assert!("\u{a66e}".to_tendril().try_subtendril(1, 2).is_err());

        assert!("\u{1f4a9}".to_tendril().try_subtendril(0, 3).is_err());
        assert!("\u{1f4a9}".to_tendril().try_subtendril(0, 2).is_err());
        assert!("\u{1f4a9}".to_tendril().try_subtendril(0, 1).is_err());
        assert!("\u{1f4a9}".to_tendril().try_subtendril(1, 3).is_err());
        assert!("\u{1f4a9}".to_tendril().try_subtendril(1, 2).is_err());
        assert!("\u{1f4a9}".to_tendril().try_subtendril(1, 1).is_err());
        assert!("\u{1f4a9}".to_tendril().try_subtendril(2, 2).is_err());
        assert!("\u{1f4a9}".to_tendril().try_subtendril(2, 1).is_err());
        assert!("\u{1f4a9}".to_tendril().try_subtendril(3, 1).is_err());

        let mut t = "\u{1f4a9}zzzzzz".to_tendril();
        assert!(t.try_pop_front(1).is_err());
        assert!(t.try_pop_front(2).is_err());
        assert!(t.try_pop_front(3).is_err());
        assert!(t.try_pop_front(4).is_ok());

        let mut t = "zzzzzz\u{1f4a9}".to_tendril();
        assert!(t.try_pop_back(1).is_err());
        assert!(t.try_pop_back(2).is_err());
        assert!(t.try_pop_back(3).is_err());
        assert!(t.try_pop_back(4).is_ok());
    }

    #[test]
    fn conversion() {
        assert_eq!(&[0x66, 0x6F, 0x6F].to_tendril(), "foo".to_tendril().as_bytes());
        assert_eq!([0x66, 0x6F, 0x6F].to_tendril(), "foo".to_tendril().into_bytes());

        let ascii: Tendril<fmt::ASCII> = b"hello".to_tendril().try_into_other_format().unwrap();
        assert_eq!(&"hello".to_tendril(), ascii.as_superset());
        assert_eq!("hello".to_tendril(), ascii.clone().into_superset());

        assert!(b"\xFF".to_tendril().try_into_other_format::<fmt::ASCII>().is_err());

        let ascii: Tendril<fmt::ASCII> = "hello".to_tendril().try_into_subset().unwrap();
        assert_eq!(b"hello", &**ascii.as_bytes());

        assert!("ő".to_tendril().try_into_other_format::<fmt::ASCII>().is_err());
    }

    #[test]
    fn clear() {
        let mut t = "foo-".to_tendril();
        t.clear();
        assert_eq!(t.len(), 0);
        assert_eq!(t.len32(), 0);
        assert_eq!(&*t, "");

        let mut t = "much longer".to_tendril();
        let s = t.clone();
        t.clear();
        assert_eq!(t.len(), 0);
        assert_eq!(t.len32(), 0);
        assert_eq!(&*t, "");
        assert_eq!(&*s, "much longer");
    }

    #[test]
    fn push_tendril() {
        let mut t = "abc".to_tendril();
        t.push_tendril(&"xyz".to_tendril());
        assert_eq!("abcxyz", &*t);
    }

    #[test]
    fn wtf8() {
        assert!(Tendril::<fmt::WTF8>::try_from_byte_slice(b"\xED\xA0\xBD").is_ok());
        assert!(Tendril::<fmt::WTF8>::try_from_byte_slice(b"\xED\xB2\xA9").is_ok());
        assert!(Tendril::<fmt::WTF8>::try_from_byte_slice(b"\xED\xA0\xBD\xED\xB2\xA9").is_err());

        let t: Tendril<fmt::WTF8>
            = Tendril::try_from_byte_slice(b"\xED\xA0\xBD\xEA\x99\xAE").unwrap();
        assert!(b"\xED\xA0\xBD".to_tendril().try_into_other_format().unwrap()
            == t.subtendril(0, 3));
        assert!(b"\xEA\x99\xAE".to_tendril().try_into_other_format().unwrap()
            == t.subtendril(3, 3));

        assert!(t.try_subtendril(0, 1).is_err());
        assert!(t.try_subtendril(0, 2).is_err());
        assert!(t.try_subtendril(1, 1).is_err());

        assert!(t.try_subtendril(3, 1).is_err());
        assert!(t.try_subtendril(3, 2).is_err());
        assert!(t.try_subtendril(4, 1).is_err());

        // paired surrogates
        let mut t: Tendril<fmt::WTF8> = Tendril::try_from_byte_slice(b"\xED\xA0\xBD").unwrap();
        assert!(t.try_push_bytes(b"\xED\xB2\xA9").is_ok());
        assert_eq!(b"\xF0\x9F\x92\xA9", t.as_byte_slice());

        // unpaired surrogates
        let mut t: Tendril<fmt::WTF8> = Tendril::try_from_byte_slice(b"\xED\xA0\xBB").unwrap();
        assert!(t.try_push_bytes(b"\xED\xA0").is_err());
        assert!(t.try_push_bytes(b"\xED").is_err());
        assert!(t.try_push_bytes(b"\xA0").is_err());
        assert!(t.try_push_bytes(b"\xED\xA0\xBD").is_ok());
        assert_eq!(b"\xED\xA0\xBB\xED\xA0\xBD", t.as_byte_slice());
        assert!(t.try_push_bytes(b"\xED\xB2\xA9").is_ok());
        assert_eq!(b"\xED\xA0\xBB\xF0\x9F\x92\xA9", t.as_byte_slice());
    }
}
