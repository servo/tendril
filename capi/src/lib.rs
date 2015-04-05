// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(core, libc)]
#![warn(warnings)]

extern crate libc;
extern crate tendril;

use tendril::{ByteTendril, StrTendril};
use std::{mem, ptr, raw};

// Link the C glue code
#[link_name="tendril_cglue"]
extern "C" { }

#[no_mangle] pub unsafe extern "C"
fn tendril_clone(t: *mut ByteTendril, r: *const ByteTendril) {
    *t = (*r).clone();
}

#[no_mangle] pub unsafe extern "C"
fn tendril_sub(t: *mut ByteTendril,
               r: *const ByteTendril,
               offset: u32,
               length: u32) {
    *t = (*r).subtendril(offset, length);
}

#[no_mangle] pub unsafe extern "C"
fn tendril_destroy(t: *mut ByteTendril) {
    ptr::read(t);
}

#[no_mangle] pub unsafe extern "C"
fn tendril_clear(t: *mut ByteTendril) {
    (*t).clear();
}

#[no_mangle] pub unsafe extern "C"
fn tendril_push_buffer(t: *mut ByteTendril, buffer: *const u8, length: u32) {
    let s = raw::Slice {
        data: buffer,
        len: length as usize,
    };
    (*t).push_slice(mem::transmute(s));
}

#[no_mangle] pub unsafe extern "C"
fn tendril_push_tendril(t: *mut ByteTendril, r: *const ByteTendril) {
    (*t).push_tendril(&*r);
}

#[no_mangle] pub unsafe extern "C"
fn tendril_push_uninit(t: *mut ByteTendril, n: u32) {
    (*t).push_uninitialized(n);
}

#[no_mangle] pub unsafe extern "C"
fn tendril_pop_front(t: *mut ByteTendril, n: u32) {
    (*t).pop_front(n);
}

#[no_mangle] pub unsafe extern "C"
fn tendril_pop_back(t: *mut ByteTendril, n: u32) {
    (*t).pop_back(n);
}

#[no_mangle] pub unsafe extern "C"
fn tendril_debug_describe(desc: *mut ByteTendril, t: *const ByteTendril) {
    use std::fmt::Write;
    let _ = write!(&mut *(desc as *mut StrTendril), "{:?}", *t);
}
