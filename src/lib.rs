// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![cfg_attr(all(test, feature = "bench"), feature(test))]
#![cfg_attr(test, deny(warnings))]

#[cfg(all(test, feature = "bench"))] extern crate test;
#[cfg(feature = "encoding")] pub use encoding;
#[cfg(feature = "encoding_rs")] pub use encoding_rs;

pub mod fmt;
pub mod stream;

mod util;
mod buf32;
mod tendril;
mod utf8_decode;

pub use crate::tendril::{Tendril, ByteTendril, StrTendril, SliceExt, ReadExt, SubtendrilError};
pub use crate::tendril::{SendTendril, Atomicity, Atomic, NonAtomic};
pub use fmt::Format;
pub use stream::TendrilSink;
pub use utf8_decode::IncompleteUtf8;

static OFLOW: &str = "tendril: overflow in buffer arithmetic";
