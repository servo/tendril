// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::{ptr, mem, intrinsics, hash, str, u32, io, slice, cmp};
use std::borrow::{Borrow, Cow};
use std::marker::PhantomData;
use std::cell::Cell;
use std::ops::{Deref, DerefMut};
use std::iter::FromIterator;
use std::io::Write;
use std::default::Default;
use std::cmp::Ordering;
use std::fmt as strfmt;

use encoding::{self, EncodingRef, DecoderTrap, EncoderTrap};

use buf32::{self, Buf32};
use fmt::{self, Slice};
use fmt::imp::Fixup;
use util::{unsafe_slice, unsafe_slice_mut, copy_and_advance, copy_lifetime_mut, copy_lifetime,
           NonZero, is_post_drop};
use OFLOW;

const MAX_INLINE_LEN: usize = 8;
const MAX_INLINE_TAG: usize = 0xF;
const EMPTY_TAG: usize = 0xF;

#[inline(always)]
fn inline_tag(len: u32) -> NonZero<usize> {
    debug_assert!(len <= MAX_INLINE_LEN as u32);
    unsafe {
        NonZero::new(if len == 0 {
            EMPTY_TAG
        } else {
            len as usize
        })
    }
}

#[repr(packed)]
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

/// Errors that can occur when slicing a `Tendril`.
#[derive(Copy, Clone, Hash, Debug, PartialEq, Eq)]
pub enum SubtendrilError {
    OutOfBounds,
    ValidationFailed,
}

/// Compact string type for zero-copy parsing.
///
/// `Tendril`s have the semantics of owned strings, but are sometimes views
/// into shared buffers. When you mutate a `Tendril`, an owned copy is made
/// if necessary. Further mutations occur in-place until the string becomes
/// shared, e.g. with `clone()` or `subtendril()`.
///
/// Buffer sharing is accomplished through thread-local (non-atomic) reference
/// counting, which has very low overhead. The Rust type system will prevent
/// you at compile time from sending a `Tendril` between threads. We plan to
/// relax this restriction in the future; see `README.md`.
///
/// Whereas `String` allocates in the heap for any non-empty string, `Tendril`
/// can store small strings (up to 8 bytes) in-line, without a heap allocation.
/// `Tendril` is also smaller than `String` on 64-bit platforms — 16 bytes
/// versus 24.
///
/// The type parameter `F` specifies the format of the tendril, for example
/// UTF-8 text or uninterpreted bytes. The parameter will be instantiated
/// with one of the marker types from `tendril::fmt`. See the `StrTendril`
/// and `ByteTendril` type aliases for two examples.
///
/// The maximum length of a `Tendril` is 4 GB. The library will panic if
/// you attempt to go over the limit.
#[cfg_attr(feature = "unstable", unsafe_no_drop_flag)]
#[repr(packed)]
pub struct Tendril<F>
    where F: fmt::Format,
{
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

impl<F> Drop for Tendril<F>
    where F: fmt::Format,
{
    #[inline]
    fn drop(&mut self) {
        unsafe {
            let p = *self.ptr.get();
            if p <= MAX_INLINE_TAG || is_post_drop(p) {
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

macro_rules! from_iter_method {
    ($ty:ty) => {
        #[inline]
        fn from_iter<I>(iterable: I) -> Self
            where I: IntoIterator<Item = $ty>
        {
            let mut output = Self::new();
            output.extend(iterable);
            output
        }
    }
}

impl Extend<char> for Tendril<fmt::UTF8> {
    #[inline]
    fn extend<I>(&mut self, iterable: I)
        where I: IntoIterator<Item = char>,
    {
        let iterator = iterable.into_iter();
        self.force_reserve(iterator.size_hint().0 as u32);
        for c in iterator {
            self.push_char(c);
        }
    }
}

impl FromIterator<char> for Tendril<fmt::UTF8> {
    from_iter_method!(char);
}

impl Extend<u8> for Tendril<fmt::Bytes> {
    #[inline]
    fn extend<I>(&mut self, iterable: I)
        where I: IntoIterator<Item = u8>,
    {
        let iterator = iterable.into_iter();
        self.force_reserve(iterator.size_hint().0 as u32);
        for b in iterator {
            self.push_slice(&[b]);
        }
    }
}

impl FromIterator<u8> for Tendril<fmt::Bytes> {
    from_iter_method!(u8);
}

impl<'a> Extend<&'a u8> for Tendril<fmt::Bytes> {
    #[inline]
    fn extend<I>(&mut self, iterable: I)
        where I: IntoIterator<Item = &'a u8>,
    {
        let iterator = iterable.into_iter();
        self.force_reserve(iterator.size_hint().0 as u32);
        for &b in iterator {
            self.push_slice(&[b]);
        }
    }
}

impl<'a> FromIterator<&'a u8> for Tendril<fmt::Bytes> {
    from_iter_method!(&'a u8);
}

impl<'a> Extend<&'a str> for Tendril<fmt::UTF8> {
    #[inline]
    fn extend<I>(&mut self, iterable: I)
        where I: IntoIterator<Item = &'a str>,
    {
        for s in iterable {
            self.push_slice(s);
        }
    }
}

impl<'a> FromIterator<&'a str> for Tendril<fmt::UTF8> {
    from_iter_method!(&'a str);
}

impl<'a> Extend<&'a [u8]> for Tendril<fmt::Bytes> {
    #[inline]
    fn extend<I>(&mut self, iterable: I)
        where I: IntoIterator<Item = &'a [u8]>,
    {
        for s in iterable {
            self.push_slice(s);
        }
    }
}

impl<'a> FromIterator<&'a [u8]> for Tendril<fmt::Bytes> {
    from_iter_method!(&'a [u8]);
}

impl<'a, F> Extend<&'a Tendril<F>> for Tendril<F>
    where F: fmt::Format + 'a,
{
    #[inline]
    fn extend<I>(&mut self, iterable: I)
        where I: IntoIterator<Item = &'a Tendril<F>>,
    {
        for t in iterable {
            self.push_tendril(t);
        }
    }
}

impl<'a, F> FromIterator<&'a Tendril<F>> for Tendril<F>
    where F: fmt::Format + 'a,
{
    from_iter_method!(&'a Tendril<F>);
}

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

impl<F> Borrow<[u8]> for Tendril<F>
    where F: fmt::SliceFormat,
{
    fn borrow(&self) -> &[u8] {
        self.as_byte_slice()
    }
}

// Why not impl Borrow<str> for Tendril<fmt::UTF8>? str and [u8] hash differently,
// and so a HashMap<StrTendril, _> would silently break if we indexed by str. Ick.
// https://github.com/rust-lang/rust/issues/27108

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
            p if p <= MAX_INLINE_TAG => "inline",
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

    /// Create a new, empty `Tendril` with a specified capacity.
    #[inline]
    pub fn with_capacity(capacity: u32) -> Tendril<F> {
        let mut t: Tendril<F> = Tendril::new();
        if capacity > MAX_INLINE_LEN as u32 {
            unsafe {
                t.make_owned_with_capacity(capacity);
            }
        }
        t
    }

    /// Reserve space for additional bytes.
    ///
    /// This is only a suggestion. There are cases where `Tendril` will
    /// decline to allocate until the buffer is actually modified.
    #[inline]
    pub fn reserve(&mut self, additional: u32) {
        if !self.is_shared() {
            // Don't grow a shared tendril because we'd have to copy
            // right away.
            self.force_reserve(additional);
        }
    }

    /// Reserve space for additional bytes, even for shared buffers.
    #[inline]
    fn force_reserve(&mut self, additional: u32) {
        let new_len = self.len32().checked_add(additional).expect(OFLOW);
        if new_len > MAX_INLINE_LEN as u32 {
            unsafe {
                self.make_owned_with_capacity(new_len);
            }
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
    #[inline]
    pub fn is_shared(&self) -> bool {
        let n = *self.ptr.get();

        (n > MAX_INLINE_TAG) && ((n & 1) == 1)
    }

    /// Is the backing buffer shared with this other `Tendril`?
    #[inline]
    pub fn is_shared_with(&self, other: &Tendril<F>) -> bool {
        let n = *self.ptr.get();

        (n > MAX_INLINE_TAG) && (n == *other.ptr.get())
    }

    /// Truncate to length 0 without discarding any owned storage.
    #[inline]
    pub fn clear(&mut self) {
        if *self.ptr.get() <= MAX_INLINE_TAG {
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
              Super: fmt::Format,
    {
        unsafe { mem::transmute(self) }
    }

    /// Convert into a superset format, for free.
    #[inline(always)]
    pub fn into_superset<Super>(self) -> Tendril<Super>
        where F: fmt::SubsetOf<Super>,
              Super: fmt::Format,
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

    /// View as another format, if the bytes of the `Tendril` are valid for
    /// that format.
    #[inline]
    pub fn try_reinterpret_view<Other>(&self) -> Result<&Tendril<Other>, ()>
        where Other: fmt::Format,
    {
        match Other::validate(self.as_byte_slice()) {
            true => Ok(unsafe { mem::transmute(self) }),
            false => Err(()),
        }
    }

    /// Convert into another format, if the `Tendril` conforms to that format.
    ///
    /// This only re-validates the existing bytes under the new format. It
    /// will *not* change the byte content of the tendril!
    ///
    /// See the `encode` and `decode` methods for character encoding conversion.
    #[inline]
    pub fn try_reinterpret<Other>(self) -> Result<Tendril<Other>, Self>
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
            if (*self.ptr.get() > MAX_INLINE_TAG) && (*other.ptr.get() > MAX_INLINE_TAG) {
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
        if n == 0 {
            return Ok(());
        }
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
    #[inline]
    pub fn pop_front(&mut self, n: u32) {
        self.try_pop_front(n).unwrap()
    }

    /// Drop `n` bytes from the back.
    ///
    /// Returns `Err` if the bytes are not available, or the prefix fails
    /// validation.
    #[inline]
    pub fn try_pop_back(&mut self, n: u32) -> Result<(), SubtendrilError> {
        if n == 0 {
            return Ok(());
        }
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
    #[inline]
    pub fn pop_back(&mut self, n: u32) {
        self.try_pop_back(n).unwrap()
    }

    /// View as another format, without validating.
    #[inline(always)]
    pub unsafe fn reinterpret_view_without_validating<Other>(&self) -> &Tendril<Other>
        where Other: fmt::Format,
    {
        mem::transmute(self)
    }

    /// Convert into another format, without validating.
    #[inline(always)]
    pub unsafe fn reinterpret_without_validating<Other>(self) -> Tendril<Other>
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
        if length <= MAX_INLINE_LEN as u32 {
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

    #[inline]
    unsafe fn incref(&self) {
        let header = self.header();
        let refcount = (*header).refcount.get().checked_add(1).expect(OFLOW);
        (*header).refcount.set(refcount);
    }

    #[inline]
    unsafe fn make_buf_shared(&self) {
        let p = *self.ptr.get();
        if p & 1 == 0 {
            let header = p as *mut Header;
            (*header).cap = self.aux.get();

            self.ptr.set(NonZero::new(p | 1));
            self.aux.set(0);
        }
    }

    #[inline]
    unsafe fn make_owned_with_capacity(&mut self, cap: u32) {
        let ptr = *self.ptr.get();
        if ptr <= MAX_INLINE_TAG || (ptr & 1) == 1 {
            *self = Tendril::owned_copy(self.as_byte_slice());
        }
        let mut buf = self.assume_buf().0;
        buf.grow(cap);
        self.ptr.set(NonZero::new(buf.ptr as usize));
        self.aux.set(buf.cap);
    }

    #[inline(always)]
    unsafe fn header(&self) -> *mut Header {
        (*self.ptr.get() & !1) as *mut Header
    }

    #[inline]
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

    #[inline]
    unsafe fn inline(x: &[u8]) -> Tendril<F> {
        let len = x.len();
        let mut t = Tendril {
            ptr: Cell::new(inline_tag(len as u32)),
            len: mem::uninitialized(),
            aux: mem::uninitialized(),
            marker: PhantomData,
        };
        intrinsics::copy_nonoverlapping(x.as_ptr(), &mut t.len as *mut u32 as *mut u8, len);
        t
    }

    #[inline]
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
        intrinsics::copy_nonoverlapping(x.as_ptr(), b.data_ptr(), x.len());
        b.len = len32;
        Tendril::owned(b)
    }

    #[inline]
    unsafe fn shared(buf: Buf32<Header>, off: u32, len: u32) -> Tendril<F> {
        Tendril {
            ptr: Cell::new(NonZero::new((buf.ptr as usize) | 1)),
            len: len,
            aux: Cell::new(off),
            marker: PhantomData,
        }
    }

    #[inline]
    fn as_byte_slice<'a>(&'a self) -> &'a [u8] {
        unsafe {
            match *self.ptr.get() {
                EMPTY_TAG => &[],
                n if n <= MAX_INLINE_LEN => {
                    slice::from_raw_parts(&self.len as *const u32 as *const u8, n)
                }
                _ => {
                    let (buf, _, offset) = self.assume_buf();
                    copy_lifetime(self, unsafe_slice(buf.data(),
                        offset as usize, self.len32() as usize))
                }
            }
        }
    }
}

impl DerefMut for Tendril<fmt::Bytes> {
    #[inline]
    fn deref_mut<'a>(&'a mut self) -> &'a mut [u8] {
        unsafe {
            match *self.ptr.get() {
                EMPTY_TAG => &mut [],
                n if n <= MAX_INLINE_LEN => {
                    slice::from_raw_parts_mut(&mut self.len as *mut u32 as *mut u8, n)
                }
                _ => {
                    let (mut buf, _, offset) = self.assume_buf();
                    let len = self.len32() as usize;
                    copy_lifetime_mut(self, unsafe_slice_mut(buf.data_mut(), offset as usize, len))
                }
            }
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
    #[inline]
    fn to_tendril(&self) -> Tendril<Self::Format> {
        Tendril::from_slice(self)
    }
}

impl SliceExt for str { }
impl SliceExt for [u8] { }

impl<F> Tendril<F>
    where F: for<'a> fmt::CharFormat<'a>,
{
    /// Remove and return the first character, if any.
    #[inline]
    pub fn pop_front_char<'a>(&'a mut self) -> Option<char> {
        unsafe {
            let mut it = F::char_indices(self.as_byte_slice());
            it.next().map(|(_, c)| {
                if let Some((n, _)) = it.next() {
                    self.unsafe_pop_front(n as u32);
                } else {
                    self.clear();
                }
                c
            })
        }
    }

    /// Remove and return a run of characters at the front of the `Tendril`
    /// which are classified the same according to the function `classify`.
    ///
    /// Returns `None` on an empty string.
    #[inline]
    pub fn pop_front_char_run<'a, C, R>(&'a mut self, mut classify: C)
        -> Option<(Tendril<F>, R)>
        where C: FnMut(char) -> R,
              R: PartialEq,
    {
        let (class, first_mismatch);
        {
            let mut chars = unsafe {
                F::char_indices(self.as_byte_slice())
            };
            let (_, first) = unwrap_or_return!(chars.next(), None);
            class = classify(first);
            first_mismatch = chars.find(|&(_, ch)| &classify(ch) != &class);
        }

        match first_mismatch {
            Some((idx, _)) => unsafe {
                let t = self.unsafe_subtendril(0, idx as u32);
                self.unsafe_pop_front(idx as u32);
                Some((t, class))
            },
            None => {
                let t = self.clone();
                self.clear();
                Some((t, class))
            }
        }
    }

    /// Push a character, if it can be represented in this format.
    #[inline]
    pub fn try_push_char(&mut self, c: char) -> Result<(), ()> {
        F::encode_char(c, |b| unsafe {
            self.push_bytes_without_validating(b);
        })
    }
}

/// Extension trait for `io::Read`.
pub trait ReadExt: io::Read {
    fn read_to_tendril(&mut self, buf: &mut Tendril<fmt::Bytes>) -> io::Result<usize>;
}

impl<T> ReadExt for T
    where T: io::Read
{
    /// Read all bytes until EOF.
    fn read_to_tendril(&mut self, buf: &mut Tendril<fmt::Bytes>) -> io::Result<usize> {
        // Adapted from libstd/io/mod.rs.
        const DEFAULT_BUF_SIZE: u32 = 64 * 1024;

        let start_len = buf.len();
        let mut len = start_len;
        let mut new_write_size = 16;
        let ret;
        loop {
            if len == buf.len() {
                if new_write_size < DEFAULT_BUF_SIZE {
                    new_write_size *= 2;
                }
                unsafe {
                    buf.push_uninitialized(new_write_size);
                }
            }

            match self.read(&mut buf[len..]) {
                Ok(0) => {
                    ret = Ok(len - start_len);
                    break;
                }
                Ok(n) => len += n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    ret = Err(e);
                    break;
                }
            }
        }

        let buf_len = buf.len32();
        buf.pop_back(buf_len - (len as u32));
        ret
    }
}

impl io::Write for Tendril<fmt::Bytes> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.push_slice(buf);
        Ok(buf.len())
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.push_slice(buf);
        Ok(())
    }

    #[inline(always)]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl encoding::ByteWriter for Tendril<fmt::Bytes> {
    #[inline]
    fn write_byte(&mut self, b: u8) {
        self.push_slice(&[b]);
    }

    #[inline]
    fn write_bytes(&mut self, v: &[u8]) {
        self.push_slice(v);
    }

    #[inline]
    fn writer_hint(&mut self, additional: usize) {
        self.reserve(cmp::min(u32::MAX as usize, additional) as u32);
    }
}

impl Tendril<fmt::Bytes> {
    /// Decode from some character encoding into UTF-8.
    ///
    /// See the [rust-encoding docs](https://lifthrasiir.github.io/rust-encoding/encoding/)
    /// for more information.
    #[inline]
    pub fn decode(&self, encoding: EncodingRef, trap: DecoderTrap)
        -> Result<Tendril<fmt::UTF8>, Cow<'static, str>>
    {
        let mut ret = Tendril::new();
        encoding.decode_to(&*self, trap, &mut ret).map(|_| ret)
    }

    /// Push "uninitialized bytes" onto the end.
    ///
    /// Really, this grows the tendril without writing anything to the new area.
    /// It's only defined for byte tendrils because it's only useful if you
    /// plan to then mutate the buffer.
    #[inline]
    pub unsafe fn push_uninitialized(&mut self, n: u32) {
        let new_len = self.len32().checked_add(n).expect(OFLOW);
        if new_len <= MAX_INLINE_LEN as u32
            && *self.ptr.get() <= MAX_INLINE_TAG
        {
            self.ptr.set(inline_tag(new_len))
        } else {
            self.make_owned_with_capacity(new_len);
            self.len = new_len;
        }
    }
}

impl strfmt::Display for Tendril<fmt::UTF8> {
    #[inline]
    fn fmt(&self, f: &mut strfmt::Formatter) -> strfmt::Result {
        <str as strfmt::Display>::fmt(&**self, f)
    }
}

impl str::FromStr for Tendril<fmt::UTF8> {
    type Err = ();

    #[inline]
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(Tendril::from_slice(s))
    }
}

impl strfmt::Write for Tendril<fmt::UTF8> {
    #[inline]
    fn write_str(&mut self, s: &str) -> strfmt::Result {
        self.push_slice(s);
        Ok(())
    }
}

impl encoding::StringWriter for Tendril<fmt::UTF8> {
    #[inline]
    fn write_char(&mut self, c: char) {
        self.push_char(c);
    }

    #[inline]
    fn write_str(&mut self, s: &str) {
        self.push_slice(s);
    }

    #[inline]
    fn writer_hint(&mut self, additional: usize) {
        self.reserve(cmp::min(u32::MAX as usize, additional) as u32);
    }
}

impl Tendril<fmt::UTF8> {
    /// Encode from UTF-8 into some other character encoding.
    ///
    /// See the [rust-encoding docs](https://lifthrasiir.github.io/rust-encoding/encoding/)
    /// for more information.
    #[inline]
    pub fn encode(&self, encoding: EncodingRef, trap: EncoderTrap)
        -> Result<Tendril<fmt::Bytes>, Cow<'static, str>>
    {
        let mut ret = Tendril::new();
        encoding.encode_to(&*self, trap, &mut ret).map(|_| ret)
    }

    /// Push a character onto the end.
    #[inline]
    pub fn push_char(&mut self, c: char) {
        unsafe {
            let mut utf_8: [u8; 4] = mem::uninitialized();
            let bytes_written = {
                let mut buffer = &mut utf_8[..];
                write!(buffer, "{}", c).ok().expect("Tendril::push_char: internal error");
                debug_assert!(buffer.len() <= 4);
                4 - buffer.len()
            };
            self.push_bytes_without_validating(unsafe_slice(&utf_8, 0, bytes_written));
        }
    }

    /// Create a `Tendril` from a single character.
    #[inline]
    pub fn from_char(c: char) -> Tendril<fmt::UTF8> {
        let mut t: Tendril<fmt::UTF8> = Tendril::new();
        t.push_char(c);
        t
    }

    /// Helper for the `format_tendril!` macro.
    #[inline]
    pub fn format(args: strfmt::Arguments) -> Tendril<fmt::UTF8> {
        use std::fmt::Write;
        let mut output: Tendril<fmt::UTF8> = Tendril::new();
        let _ = write!(&mut output, "{}", args);
        output
    }
}

/// Create a `StrTendril` through string formatting.
///
/// Works just like the standard `format!` macro.
#[macro_export]
macro_rules! format_tendril {
    ($($arg:tt)*) => ($crate::Tendril::format(format_args!($($arg)*)))
}


impl<'a, F> From<&'a F::Slice> for Tendril<F> where F: fmt::SliceFormat {
    #[inline]
    fn from(input: &F::Slice) -> Tendril<F> {
        Tendril::from_slice(input)
    }
}

impl From<String> for Tendril<fmt::UTF8> {
    #[inline]
    fn from(input: String) -> Tendril<fmt::UTF8> {
        Tendril::from_slice(&*input)
    }
}

impl<F> AsRef<F::Slice> for Tendril<F> where F: fmt::SliceFormat {
    #[inline]
    fn as_ref(&self) -> &F::Slice {
        &**self
    }
}

impl From<Tendril<fmt::UTF8>> for String {
    #[inline]
    fn from(input: Tendril<fmt::UTF8>) -> String {
        String::from(&*input)
    }
}

impl<'a> From<&'a Tendril<fmt::UTF8>> for String {
    #[inline]
    fn from(input: &'a Tendril<fmt::UTF8>) -> String {
        String::from(&**input)
    }
}


#[cfg(all(test, feature = "unstable"))]
#[path="bench.rs"]
mod bench;

#[cfg(test)]
mod test {
    use super::{Tendril, ByteTendril, StrTendril, ReadExt, SliceExt, Header};
    use fmt;
    use std::iter;

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
        let drop_flag = if cfg!(feature = "unstable") { 0 } else { 1 };
        let correct = mem::size_of::<*const ()>() + 8 + drop_flag;

        assert_eq!(correct, mem::size_of::<ByteTendril>());
        assert_eq!(correct, mem::size_of::<StrTendril>());

        // Check that the NonZero<T> optimization is working, if on unstable Rust.
        let option_tag = if cfg!(feature = "unstable") { 0 } else { 1 };
        let correct = correct + option_tag;
        assert_eq!(correct, mem::size_of::<Option<ByteTendril>>());
        assert_eq!(correct, mem::size_of::<Option<StrTendril>>());

        let correct_header = mem::size_of::<*const ()>() + 4;
        assert_eq!(correct_header, mem::size_of::<Header>());
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
        assert_eq!("zzzzzz", &*t);

        let mut t = "zzzzzz\u{1f4a9}".to_tendril();
        assert!(t.try_pop_back(1).is_err());
        assert!(t.try_pop_back(2).is_err());
        assert!(t.try_pop_back(3).is_err());
        assert!(t.try_pop_back(4).is_ok());
        assert_eq!("zzzzzz", &*t);
    }

    #[test]
    fn conversion() {
        assert_eq!(&[0x66, 0x6F, 0x6F].to_tendril(), "foo".to_tendril().as_bytes());
        assert_eq!([0x66, 0x6F, 0x6F].to_tendril(), "foo".to_tendril().into_bytes());

        let ascii: Tendril<fmt::ASCII> = b"hello".to_tendril().try_reinterpret().unwrap();
        assert_eq!(&"hello".to_tendril(), ascii.as_superset());
        assert_eq!("hello".to_tendril(), ascii.clone().into_superset());

        assert!(b"\xFF".to_tendril().try_reinterpret::<fmt::ASCII>().is_err());

        let t = "hello".to_tendril();
        let ascii: &Tendril<fmt::ASCII> = t.try_as_subset().unwrap();
        assert_eq!(b"hello", &**ascii.as_bytes());

        assert!("ő".to_tendril().try_reinterpret_view::<fmt::ASCII>().is_err());
        assert!("ő".to_tendril().try_as_subset::<fmt::ASCII>().is_err());

        let ascii: Tendril<fmt::ASCII> = "hello".to_tendril().try_into_subset().unwrap();
        assert_eq!(b"hello", &**ascii.as_bytes());

        assert!("ő".to_tendril().try_reinterpret::<fmt::ASCII>().is_err());
        assert!("ő".to_tendril().try_into_subset::<fmt::ASCII>().is_err());
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
        assert!(b"\xED\xA0\xBD".to_tendril().try_reinterpret().unwrap()
            == t.subtendril(0, 3));
        assert!(b"\xEA\x99\xAE".to_tendril().try_reinterpret().unwrap()
            == t.subtendril(3, 3));
        assert!(t.try_reinterpret_view::<fmt::UTF8>().is_err());

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
        assert!(t.try_reinterpret_view::<fmt::UTF8>().is_ok());

        // unpaired surrogates
        let mut t: Tendril<fmt::WTF8> = Tendril::try_from_byte_slice(b"\xED\xA0\xBB").unwrap();
        assert!(t.try_push_bytes(b"\xED\xA0").is_err());
        assert!(t.try_push_bytes(b"\xED").is_err());
        assert!(t.try_push_bytes(b"\xA0").is_err());
        assert!(t.try_push_bytes(b"\xED\xA0\xBD").is_ok());
        assert_eq!(b"\xED\xA0\xBB\xED\xA0\xBD", t.as_byte_slice());
        assert!(t.try_push_bytes(b"\xED\xB2\xA9").is_ok());
        assert_eq!(b"\xED\xA0\xBB\xF0\x9F\x92\xA9", t.as_byte_slice());
        assert!(t.try_reinterpret_view::<fmt::UTF8>().is_err());
    }

    #[test]
    fn front_char() {
        let mut t = "".to_tendril();
        assert_eq!(None, t.pop_front_char());
        assert_eq!(None, t.pop_front_char());

        let mut t = "abc".to_tendril();
        assert_eq!(Some('a'), t.pop_front_char());
        assert_eq!(Some('b'), t.pop_front_char());
        assert_eq!(Some('c'), t.pop_front_char());
        assert_eq!(None, t.pop_front_char());
        assert_eq!(None, t.pop_front_char());

        let mut t = "főo-a-longer-string-bar-baz".to_tendril();
        assert_eq!(28, t.len());
        assert_eq!(Some('f'), t.pop_front_char());
        assert_eq!(Some('ő'), t.pop_front_char());
        assert_eq!(Some('o'), t.pop_front_char());
        assert_eq!(Some('-'), t.pop_front_char());
        assert_eq!(23, t.len());
    }

    #[test]
    fn char_run() {
        for &(s, exp) in &[
            ("", None),
            (" ", Some((" ", true))),
            ("x", Some(("x", false))),
            ("  \t  \n", Some(("  \t  \n", true))),
            ("xyzzy", Some(("xyzzy", false))),
            ("   xyzzy", Some(("   ", true))),
            ("xyzzy   ", Some(("xyzzy", false))),
            ("   xyzzy  ", Some(("   ", true))),
            ("xyzzy   hi", Some(("xyzzy", false))),
            ("中 ", Some(("中", false))),
            (" 中 ", Some((" ", true))),
            ("  中 ", Some(("  ", true))),
            ("   中 ", Some(("   ", true))),
        ] {
            let mut t = s.to_tendril();
            let res = t.pop_front_char_run(char::is_whitespace);
            match exp {
                None => assert!(res.is_none()),
                Some((es, ec)) => {
                    let (rt, rc) = res.unwrap();
                    assert_eq!(es, &*rt);
                    assert_eq!(ec, rc);
                }
            }
        }
    }

    #[test]
    fn deref_mut() {
        let mut t = "xyő".to_tendril().into_bytes();
        t[3] = 0xff;
        assert_eq!(b"xy\xC5\xFF", &*t);
        assert!(t.try_reinterpret_view::<fmt::UTF8>().is_err());
        t[3] = 0x8b;
        assert_eq!("xyŋ", &**t.try_reinterpret_view::<fmt::UTF8>().unwrap());

        unsafe {
            t.push_uninitialized(3);
            t[4] = 0xEA;
            t[5] = 0x99;
            t[6] = 0xAE;
            assert_eq!("xyŋ\u{a66e}", &**t.try_reinterpret_view::<fmt::UTF8>().unwrap());
            t.push_uninitialized(20);
            t.pop_back(20);
            assert_eq!("xyŋ\u{a66e}", &**t.try_reinterpret_view::<fmt::UTF8>().unwrap());
        }
    }

    #[test]
    fn push_char() {
        let mut t = "xyz".to_tendril();
        t.push_char('o');
        assert_eq!("xyzo", &*t);
        t.push_char('ő');
        assert_eq!("xyzoő", &*t);
        t.push_char('\u{a66e}');
        assert_eq!("xyzoő\u{a66e}", &*t);
        t.push_char('\u{1f4a9}');
        assert_eq!("xyzoő\u{a66e}\u{1f4a9}", &*t);
        assert_eq!(t.len(), 13);
    }

    #[test]
    fn encode() {
        use encoding::{all, EncoderTrap};

        let t = "안녕하세요 러스트".to_tendril();
        assert_eq!(b"\xbe\xc8\xb3\xe7\xc7\xcf\xbc\xbc\xbf\xe4\x20\xb7\xaf\xbd\xba\xc6\xae",
            &*t.encode(all::WINDOWS_949, EncoderTrap::Strict).unwrap());

        let t = "Энергия пробуждения ия-я-я! \u{a66e}".to_tendril();
        assert_eq!(b"\xfc\xce\xc5\xd2\xc7\xc9\xd1 \xd0\xd2\xcf\xc2\xd5\xd6\xc4\xc5\xce\
                     \xc9\xd1 \xc9\xd1\x2d\xd1\x2d\xd1\x21 ?",
            &*t.encode(all::KOI8_U, EncoderTrap::Replace).unwrap());

        let t = "\u{1f4a9}".to_tendril();
        assert!(t.encode(all::WINDOWS_1252, EncoderTrap::Strict).is_err());
    }

    #[test]
    fn decode() {
        use encoding::{all, DecoderTrap};

        let t = b"\xbe\xc8\xb3\xe7\xc7\xcf\xbc\xbc\
                  \xbf\xe4\x20\xb7\xaf\xbd\xba\xc6\xae".to_tendril();
        assert_eq!("안녕하세요 러스트",
            &*t.decode(all::WINDOWS_949, DecoderTrap::Strict).unwrap());

        let t = b"\xfc\xce\xc5\xd2\xc7\xc9\xd1 \xd0\xd2\xcf\xc2\xd5\xd6\xc4\xc5\xce\
                  \xc9\xd1 \xc9\xd1\x2d\xd1\x2d\xd1\x21".to_tendril();
        assert_eq!("Энергия пробуждения ия-я-я!",
            &*t.decode(all::KOI8_U, DecoderTrap::Replace).unwrap());

        let t = b"x \xff y".to_tendril();
        assert!(t.decode(all::UTF_8, DecoderTrap::Strict).is_err());

        let t = b"x \xff y".to_tendril();
        assert_eq!("x \u{fffd} y",
            &*t.decode(all::UTF_8, DecoderTrap::Replace).unwrap());
    }

    #[test]
    fn ascii() {
        fn mk(x: &[u8]) -> Tendril<fmt::ASCII> {
            x.to_tendril().try_reinterpret().unwrap()
        }

        let mut t = mk(b"xyz");
        assert_eq!(Some('x'), t.pop_front_char());
        assert_eq!(Some('y'), t.pop_front_char());
        assert_eq!(Some('z'), t.pop_front_char());
        assert_eq!(None, t.pop_front_char());

        let mut t = mk(b" \t xyz");
        assert!(Some((mk(b" \t "), true))
            == t.pop_front_char_run(char::is_whitespace));
        assert!(Some((mk(b"xyz"), false))
            == t.pop_front_char_run(char::is_whitespace));
        assert!(t.pop_front_char_run(char::is_whitespace).is_none());

        let mut t = Tendril::<fmt::ASCII>::new();
        assert!(t.try_push_char('x').is_ok());
        assert!(t.try_push_char('\0').is_ok());
        assert!(t.try_push_char('\u{a0}').is_err());
        assert_eq!(b"x\0", t.as_byte_slice());
    }

    #[test]
    fn latin1() {
        fn mk(x: &[u8]) -> Tendril<fmt::Latin1> {
            x.to_tendril().try_reinterpret().unwrap()
        }

        let mut t = mk(b"\xd8_\xd8");
        assert_eq!(Some('Ø'), t.pop_front_char());
        assert_eq!(Some('_'), t.pop_front_char());
        assert_eq!(Some('Ø'), t.pop_front_char());
        assert_eq!(None, t.pop_front_char());

        let mut t = mk(b" \t \xfe\xa7z");
        assert!(Some((mk(b" \t "), true))
            == t.pop_front_char_run(char::is_whitespace));
        assert!(Some((mk(b"\xfe\xa7z"), false))
            == t.pop_front_char_run(char::is_whitespace));
        assert!(t.pop_front_char_run(char::is_whitespace).is_none());

        let mut t = Tendril::<fmt::Latin1>::new();
        assert!(t.try_push_char('x').is_ok());
        assert!(t.try_push_char('\0').is_ok());
        assert!(t.try_push_char('\u{a0}').is_ok());
        assert!(t.try_push_char('ő').is_err());
        assert!(t.try_push_char('я').is_err());
        assert!(t.try_push_char('\u{a66e}').is_err());
        assert!(t.try_push_char('\u{1f4a9}').is_err());
        assert_eq!(b"x\0\xa0", t.as_byte_slice());
    }

    #[test]
    fn format() {
        assert_eq!("", &*format_tendril!(""));
        assert_eq!("two and two make 4", &*format_tendril!("two and two make {}", 2+2));
    }

    #[test]
    fn merge_shared() {
        let t = "012345678901234567890123456789".to_tendril();
        let a = t.subtendril(10, 20);
        assert!(a.is_shared());
        assert_eq!("01234567890123456789", &*a);
        let mut b = t.subtendril(0, 10);
        assert!(b.is_shared());
        assert_eq!("0123456789", &*b);

        b.push_tendril(&a);
        assert!(b.is_shared());
        assert!(a.is_shared());
        assert!(a.is_shared_with(&b));
        assert!(b.is_shared_with(&a));
        assert_eq!("012345678901234567890123456789", &*b);

        assert!(t.is_shared());
        assert!(t.is_shared_with(&a));
        assert!(t.is_shared_with(&b));
    }

    #[test]
    fn merge_cant_share() {
        let t = "012345678901234567890123456789".to_tendril();
        let mut b = t.subtendril(0, 10);
        assert!(b.is_shared());
        assert_eq!("0123456789", &*b);

        b.push_tendril(&"abcd".to_tendril());
        assert!(!b.is_shared());
        assert_eq!("0123456789abcd", &*b);
    }

    #[test]
    fn shared_doesnt_reserve() {
        let mut t = "012345678901234567890123456789".to_tendril();
        let a = t.subtendril(1, 10);

        assert!(t.is_shared());
        t.reserve(10);
        assert!(t.is_shared());

        let _ = a;
    }

    #[test]
    fn out_of_bounds() {
        assert!("".to_tendril().try_subtendril(0, 1).is_err());
        assert!("abc".to_tendril().try_subtendril(0, 4).is_err());
        assert!("abc".to_tendril().try_subtendril(3, 1).is_err());
        assert!("abc".to_tendril().try_subtendril(7, 1).is_err());

        let mut t = "".to_tendril();
        assert!(t.try_pop_front(1).is_err());
        assert!(t.try_pop_front(5).is_err());
        assert!(t.try_pop_front(500).is_err());
        assert!(t.try_pop_back(1).is_err());
        assert!(t.try_pop_back(5).is_err());
        assert!(t.try_pop_back(500).is_err());


        let mut t = "abcd".to_tendril();
        assert!(t.try_pop_front(1).is_ok());
        assert!(t.try_pop_front(4).is_err());
        assert!(t.try_pop_front(500).is_err());
        assert!(t.try_pop_back(1).is_ok());
        assert!(t.try_pop_back(3).is_err());
        assert!(t.try_pop_back(500).is_err());
    }

    #[test]
    fn compare() {
        for &a in &["indiscretions", "validity", "hallucinogenics", "timelessness",
                    "original", "microcosms", "boilers", "mammoth"] {
            for &b in &["intrepidly", "frigid", "spa", "cardigans",
                        "guileful", "evaporated", "unenthusiastic", "legitimate"] {
                let ta = a.to_tendril();
                let tb = b.to_tendril();

                assert_eq!(a.eq(b), ta.eq(&tb));
                assert_eq!(a.ne(b), ta.ne(&tb));
                assert_eq!(a.lt(b), ta.lt(&tb));
                assert_eq!(a.le(b), ta.le(&tb));
                assert_eq!(a.gt(b), ta.gt(&tb));
                assert_eq!(a.ge(b), ta.ge(&tb));
                assert_eq!(a.partial_cmp(b), ta.partial_cmp(&tb));
                assert_eq!(a.cmp(b), ta.cmp(&tb));
            }
        }
    }

    #[test]
    fn extend_and_from_iterator() {
        // Testing Extend<T> and FromIterator<T> for the various Ts.

        // Tendril<F>
        let mut t = "Hello".to_tendril();
        t.extend(None::<&Tendril<_>>.into_iter());
        assert_eq!("Hello", &*t);
        t.extend(&[", ".to_tendril(), "world".to_tendril(), "!".to_tendril()]);
        assert_eq!("Hello, world!", &*t);
        assert_eq!("Hello, world!", &*["Hello".to_tendril(), ", ".to_tendril(),
                                       "world".to_tendril(), "!".to_tendril()]
                                    .iter().collect::<StrTendril>());

        // &str
        let mut t = "Hello".to_tendril();
        t.extend(None::<&str>.into_iter());
        assert_eq!("Hello", &*t);
        t.extend([", ", "world", "!"].iter().map(|&s| s));
        assert_eq!("Hello, world!", &*t);
        assert_eq!("Hello, world!", &*["Hello", ", ", "world", "!"]
                                    .iter().map(|&s| s).collect::<StrTendril>());

        // &[u8]
        let mut t = b"Hello".to_tendril();
        t.extend(None::<&[u8]>.into_iter());
        assert_eq!(b"Hello", &*t);
        t.extend([b", " as &[u8], b"world" as &[u8], b"!" as &[u8]].iter().map(|&s| s));
        assert_eq!(b"Hello, world!", &*t);
        assert_eq!(b"Hello, world!", &*[b"Hello" as &[u8], b", " as &[u8],
                                        b"world" as &[u8], b"!" as &[u8]]
                                    .iter().map(|&s| s).collect::<ByteTendril>());

        let string = "the quick brown fox jumps over the lazy dog";
        let string_expected = string.to_tendril();
        let bytes = string.as_bytes();
        let bytes_expected = bytes.to_tendril();

        // char
        assert_eq!(string_expected, string.chars().collect());
        let mut tendril = StrTendril::new();
        tendril.extend(string.chars());
        assert_eq!(string_expected, tendril);

        // &u8
        assert_eq!(bytes_expected, bytes.iter().collect());
        let mut tendril = ByteTendril::new();
        tendril.extend(bytes);
        assert_eq!(bytes_expected, tendril);

        // u8
        assert_eq!(bytes_expected, bytes.iter().map(|&b| b).collect());
        let mut tendril = ByteTendril::new();
        tendril.extend(bytes.iter().map(|&b| b));
        assert_eq!(bytes_expected, tendril);
    }

    #[test]
    fn from_str() {
        use std::str::FromStr;
        let t: Tendril<_> = FromStr::from_str("foo bar baz").unwrap();
        assert_eq!("foo bar baz", &*t);
    }

    #[test]
    fn from_char() {
        assert_eq!("o", &*StrTendril::from_char('o'));
        assert_eq!("ő", &*StrTendril::from_char('ő'));
        assert_eq!("\u{a66e}", &*StrTendril::from_char('\u{a66e}'));
        assert_eq!("\u{1f4a9}", &*StrTendril::from_char('\u{1f4a9}'));
    }

    #[test]
    fn read() {
        fn check(x: &[u8]) {
            use std::io::Cursor;
            let mut t = Tendril::new();
            assert_eq!(x.len(), Cursor::new(x).read_to_tendril(&mut t).unwrap());
            assert_eq!(x, &*t);
        }

        check(b"");
        check(b"abcd");

        let long: Vec<u8> = iter::repeat(b'x').take(1_000_000).collect();
        check(&long);
    }

    #[test]
    fn hash_map_key() {
        use std::collections::HashMap;

        // As noted with Borrow, indexing on HashMap<StrTendril, _> is byte-based because of
        // https://github.com/rust-lang/rust/issues/27108.
        let mut map = HashMap::new();
        map.insert("foo".to_tendril(), 1);
        assert_eq!(map.get(b"foo" as &[u8]), Some(&1));
        assert_eq!(map.get(b"bar" as &[u8]), None);

        let mut map = HashMap::new();
        map.insert(b"foo".to_tendril(), 1);
        assert_eq!(map.get(b"foo" as &[u8]), Some(&1));
        assert_eq!(map.get(b"bar" as &[u8]), None);
    }
}
