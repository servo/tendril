// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! A simple fuzz tester for the library.
#![no_main]
#![deny(warnings)]
#[macro_use]
extern crate libfuzzer_sys;

extern crate tendril;
extern crate rand;

use rand::Rng;
use tendril::StrTendril;
use std::convert::TryInto;
use rand::distributions::{IndependentSample, Range};


fuzz_target!(|data: &[u8]| {
    // prelude
    let capacity= data.len();
    let mut buf_string = String::with_capacity(capacity as usize);
    let mut buf_tendril = StrTendril::with_capacity(capacity.try_into().unwrap());
    if let Ok(str) = std::str::from_utf8(&data) {
    buf_string.push_str(&str);
    buf_tendril.push_slice(&str);

    // test subtendril
    let mut rng = rand::thread_rng();
    let (start, end) = random_slice(&mut rng, &buf_string);
    buf_string = buf_string[start..end].to_owned();
    buf_tendril = buf_tendril.subtendril(start as u32, (end - start) as u32);
    assert_eq!(&*buf_string, &*buf_tendril);

  }
});

fn random_slice<R: Rng>(rng: &mut R, text: &str) -> (usize, usize) {
    loop {
        let start = Range::new(0, text.len() + 1).ind_sample(rng);
        let end = Range::new(start, text.len() + 1).ind_sample(rng);
        if !text.is_char_boundary(start) {
            continue;
        }
        if end < text.len() && !text.is_char_boundary(end) {
            continue;
        }
        return (start, end);
    }
}
