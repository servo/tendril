// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![deny(
    rust_2018_compatibility,
    rust_2018_idioms,
    future_incompatible,
    nonstandard_style,
    unused,
    missing_copy_implementations,
    missing_abi,
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::wildcard_imports,
    clippy::cloned_instead_of_copied,
    clippy::unreadable_literal,
    clippy::unseparated_literal_suffix
)]
#![cfg_attr(all(test, feature = "bench"), feature(test))]
//#![cfg_attr(test, deny(warnings))]

#[cfg(feature = "encoding")]
pub extern crate encoding;
#[cfg(feature = "encoding_rs")]
pub extern crate encoding_rs;
#[cfg(all(test, feature = "bench"))]
extern crate test;
#[macro_use]
extern crate mac;

pub use crate::fmt::Format;
pub use crate::stream::TendrilSink;
pub use crate::tendril::{Atomic, Atomicity, NonAtomic, SendTendril};
pub use crate::tendril::{ByteTendril, ReadExt, SliceExt, StrTendril, SubtendrilError, Tendril};
pub use crate::utf8_decode::IncompleteUtf8;

pub mod fmt;
pub mod stream;

mod buf32;
mod tendril;
mod utf8_decode;
mod util;

static OFLOW: &str = "tendril: overflow in buffer arithmetic";
