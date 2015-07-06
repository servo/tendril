// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(alloc, core, unsafe_no_drop_flag, filling_drop, unicode, ref_slice, nonzero, heap_api, oom)]
#![cfg_attr(test, feature(test, str_char))]
#![deny(warnings)]

extern crate alloc;
extern crate core;
#[macro_use] extern crate mac;
extern crate futf;
extern crate encoding;

#[cfg(test)]
extern crate test;

pub use tendril::{Tendril, ByteTendril, StrTendril, SliceExt, ReadExt, SubtendrilError};
pub use fmt::Format;

pub mod fmt;
pub mod stream;

mod util;
mod buf32;
mod tendril;

static OFLOW: &'static str = "tendril: overflow in buffer arithmetic";
