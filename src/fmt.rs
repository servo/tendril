// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Marker types for formats.
//!
//! This module defines the types and traits used to mark a `Tendril`
//! with the format of data it contains.
//!
//! `Tendril` operations may become memory-unsafe if data invalid for
//! the format sneaks in. For that reason, these traits require
//! `unsafe impl`.

use std::{char, str, mem};
use std::marker::{MarkerTrait, PhantomFn};
use std::default::Default;

use futf::{self, Codepoint, Meaning};

/// Implementation details.
///
/// You don't need these unless you are implementing
/// a new format.
pub mod imp {
    use std::default::Default;

    /// Describes how to fix up encodings when concatenating.
    ///
    /// We can drop characters on either side of the splice,
    /// and insert up to 4 bytes in the middle.
    pub struct Fixup {
        pub drop_left: u32,
        pub drop_right: u32,
        pub insert_len: u32,
        pub insert_bytes: [u8; 4],
    }

    impl Default for Fixup {
        #[inline(always)]
        fn default() -> Fixup {
            Fixup {
                drop_left: 0,
                drop_right: 0,
                insert_len: 0,
                insert_bytes: [0; 4],
            }
        }
    }
}

/// Trait for format marker types.
///
/// The type implementing this trait is usually not instantiated.
/// It's used with a phantom type parameter of `Tendril`.
pub unsafe trait Format: MarkerTrait {
    /// Check whether the buffer is valid for this format.
    fn validate(buf: &[u8]) -> bool;

    /// Check whether the buffer is valid for this format.
    ///
    /// You may assume the buffer is a prefix of a valid buffer.
    #[inline]
    fn validate_prefix(buf: &[u8]) -> bool {
        <Self as Format>::validate(buf)
    }

    /// Check whether the buffer is valid for this format.
    ///
    /// You may assume the buffer is a suffix of a valid buffer.
    #[inline]
    fn validate_suffix(buf: &[u8]) -> bool {
        <Self as Format>::validate(buf)
    }

    /// Check whether the buffer is valid for this format.
    ///
    /// You may assume the buffer is a contiguous subsequence
    /// of a valid buffer, but not necessarily a prefix or
    /// a suffix.
    #[inline]
    fn validate_subseq(buf: &[u8]) -> bool {
        <Self as Format>::validate(buf)
    }

    /// Compute any fixup needed when concatenating buffers.
    ///
    /// The default is to do nothing.
    ///
    /// The function is `unsafe` because it may assume the input
    /// buffers are already valid for the format. Also, no
    /// bounds-checking is performed on the return value!
    #[inline(always)]
    unsafe fn fixup(_lhs: &[u8], _rhs: &[u8]) -> imp::Fixup {
        Default::default()
    }
}

/// Indicates that one format is a subset of another.
///
/// The subset format can be converted to the superset format
/// for free.
pub unsafe trait SubsetOf<Super>: Format + PhantomFn<Super>
    where Super: Format,
{
    /// Validate the *other* direction of conversion; check if
    /// this buffer from the superset format conforms to the
    /// subset format.
    ///
    /// The default calls `Self::validate`, but some conversions
    /// may implement a check which is cheaper than validating
    /// from scratch.
    fn revalidate_subset(x: &[u8]) -> bool {
        Self::validate(x)
    }
}

/// Indicates a format which corresponds to a Rust slice type,
/// representing exactly the same invariants.
pub unsafe trait SliceFormat: Format {
    type Slice: ?Sized + Slice<Format = Self>;
}

/// Indicates a Rust slice type that has a corresponding format.
pub unsafe trait Slice {
    type Format: SliceFormat<Slice = Self>;

    /// Access the raw bytes of the slice.
    fn as_bytes(&self) -> &[u8];

    /// Convert a byte slice to this kind of slice.
    ///
    /// You may assume the buffer is *already validated*
    /// for `Format`.
    unsafe fn from_bytes(x: &[u8]) -> &Self;
}

/// Marker type for uninterpreted bytes.
///
/// Validation will never fail for this format.
#[derive(Copy, Clone, Default, Debug)]
pub struct Bytes;

unsafe impl Format for Bytes {
    #[inline(always)]
    fn validate(_: &[u8]) -> bool {
        true
    }
}

unsafe impl SliceFormat for Bytes {
    type Slice = [u8];
}

unsafe impl Slice for [u8] {
    type Format = Bytes;

    #[inline(always)]
    fn as_bytes(&self) -> &[u8] {
        self
    }

    #[inline(always)]
    unsafe fn from_bytes(x: &[u8]) -> &[u8] {
        x
    }
}

/// Marker type for ASCII text.
#[derive(Copy, Clone, Default, Debug)]
pub struct ASCII;

unsafe impl Format for ASCII {
    #[inline]
    fn validate(buf: &[u8]) -> bool {
        buf.iter().all(|&n| n <= 127)
    }

    #[inline(always)]
    fn validate_prefix(_: &[u8]) -> bool {
        true
    }

    #[inline(always)]
    fn validate_suffix(_: &[u8]) -> bool {
        true
    }

    #[inline(always)]
    fn validate_subseq(_: &[u8]) -> bool {
        true
    }
}

unsafe impl SubsetOf<UTF8> for ASCII { }

/// Marker type for UTF-8 text.
#[derive(Copy, Clone, Default, Debug)]
pub struct UTF8;

unsafe impl Format for UTF8 {
    #[inline]
    fn validate(buf: &[u8]) -> bool {
        str::from_utf8(buf).is_ok()
    }

    #[inline]
    fn validate_prefix(buf: &[u8]) -> bool {
        if buf.len() == 0 {
            return true;
        }
        match futf::classify(buf, buf.len() - 1) {
            Some(Codepoint { meaning: Meaning::Whole(_), .. }) => true,
            _ => false,
        }
    }

    #[inline]
    fn validate_suffix(buf: &[u8]) -> bool {
        if buf.len() == 0 {
            return true;
        }
        match futf::classify(buf, 0) {
            Some(Codepoint { meaning: Meaning::Whole(_), .. }) => true,
            _ => false,
        }
    }

    #[inline]
    fn validate_subseq(buf: &[u8]) -> bool {
        <Self as Format>::validate_prefix(buf)
            && <Self as Format>::validate_suffix(buf)
    }
}

unsafe impl SubsetOf<WTF8> for UTF8 { }

unsafe impl SliceFormat for UTF8 {
    type Slice = str;
}

unsafe impl Slice for str {
    type Format = UTF8;

    #[inline(always)]
    fn as_bytes(&self) -> &[u8] {
        str::as_bytes(self)
    }

    #[inline(always)]
    unsafe fn from_bytes(x: &[u8]) -> &str {
        str::from_utf8_unchecked(x)
    }
}

/// Marker type for WTF-8 text.
///
/// See the [WTF-8 spec](http://simonsapin.github.io/wtf-8/).
#[derive(Copy, Clone, Default, Debug)]
pub struct WTF8;

#[inline]
fn wtf8_meaningful(m: Meaning) -> bool {
    match m {
        Meaning::Whole(_) | Meaning::LeadSurrogate(_)
            | Meaning::TrailSurrogate(_) => true,
        _ => false,
    }
}

unsafe impl Format for WTF8 {
    #[inline]
    fn validate(buf: &[u8]) -> bool {
        let mut i = 0;
        let mut prev_lead = false;
        while i < buf.len() {
            let codept = unwrap_or_return!(futf::classify(buf, i), false);
            if !wtf8_meaningful(codept.meaning) {
                return false;
            }
            i += codept.bytes.len();
            prev_lead = match codept.meaning {
                Meaning::TrailSurrogate(_) if prev_lead => return false,
                Meaning::LeadSurrogate(_) => true,
                _ => false,
            };
        }

        true
    }

    #[inline]
    fn validate_prefix(buf: &[u8]) -> bool {
        if buf.len() == 0 {
            return true;
        }
        match futf::classify(buf, buf.len() - 1) {
            Some(c) => wtf8_meaningful(c.meaning),
            _ => false,
        }
    }

    #[inline]
    fn validate_suffix(buf: &[u8]) -> bool {
        if buf.len() == 0 {
            return true;
        }
        match futf::classify(buf, 0) {
            Some(c) => wtf8_meaningful(c.meaning),
            _ => false,
        }
    }

    #[inline]
    fn validate_subseq(buf: &[u8]) -> bool {
        <Self as Format>::validate_prefix(buf)
            && <Self as Format>::validate_suffix(buf)
    }

    #[inline]
    unsafe fn fixup(lhs: &[u8], rhs: &[u8]) -> imp::Fixup {
        const ERR: &'static str = "WTF8: internal error";

        if lhs.len() >= 3 && rhs.len() >= 3 {
            if let (Some(Codepoint { meaning: Meaning::LeadSurrogate(hi), .. }),
                    Some(Codepoint { meaning: Meaning::TrailSurrogate(lo), .. }))
                = (futf::classify(lhs, lhs.len() - 1), futf::classify(rhs, 0))
            {
                let mut fixup = imp::Fixup {
                    drop_left: 3,
                    drop_right: 3,
                    insert_len: 0,
                    insert_bytes: mem::uninitialized(),
                };

                let n = 0x10000 + ((hi as u32) << 10) + (lo as u32);
                fixup.insert_len = char::from_u32(n).expect(ERR)
                    .encode_utf8(&mut fixup.insert_bytes).expect(ERR)
                    as u32;
                return fixup;
            }
        }

        Default::default()
    }
}
