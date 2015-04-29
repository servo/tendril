// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Streams of tendrils.

use tendril::Tendril;
use fmt;

use std::{cmp, mem};
use std::borrow::Cow;

use encoding::{self, EncodingRef, RawDecoder, DecoderTrap};
use futf::{self, Codepoint, Meaning};

/// Trait for types that can process a tendril.
///
/// This is a "push" interface, unlike the "pull" interface of
/// `Iterator<Item=Tendril<F>>`. The push interface matches
/// [html5ever][] and other incremental parsers with a similar
/// architecture.
///
/// [html5ever]: https://github.com/servo/html5ever
pub trait TendrilSink<F>
    where F: fmt::Format,
{
    /// Process this tendril.
    fn process(&mut self, t: Tendril<F>);

    /// Indicates the end of the stream.
    ///
    /// By default, does nothing.
    fn finish(&mut self) { }

    /// Indicates that an error has occurred.
    fn error(&mut self, desc: Cow<'static, str>);
}

/// Incrementally validate a byte stream as UTF-8.
///
/// This will copy as little as possible — only the characters that
/// span a chunk boundary.
pub struct UTF8Validator<Sink> {
    pfx: Tendril<fmt::Bytes>,
    need: usize,
    sink: Sink,
}

impl<Sink> UTF8Validator<Sink>
    where Sink: TendrilSink<fmt::UTF8>,
{
    /// Create a new incremental validator.
    #[inline]
    pub fn new(sink: Sink) -> UTF8Validator<Sink> {
        UTF8Validator {
            pfx: Tendril::new(),
            need: 0,
            sink: sink,
        }
    }

    /// Consume the validator and obtain the sink.
    #[inline]
    pub fn into_sink(self) -> Sink {
        self.sink
    }

    fn emit_char(&mut self, c: char) {
        let mut t: Tendril<fmt::UTF8> = Tendril::new();
        t.push_char(c);
        self.sink.process(t);
    }
}

impl<Sink> TendrilSink<fmt::Bytes> for UTF8Validator<Sink>
    where Sink: TendrilSink<fmt::UTF8>,
{
    #[inline]
    fn process(&mut self, mut t: Tendril<fmt::Bytes>) {
        const INVALID: &'static str = "invalid byte sequence(s)";

        let cont = cmp::min(self.need, t.len());
        if cont > 0 {
            self.pfx.push_slice(&t[..cont]);
            t.pop_front(cont as u32);
            self.need -= cont;
        }

        if self.need > 0 {
            return;
        }

        if self.pfx.len() > 0 {
            let pfx = mem::replace(&mut self.pfx, Tendril::new());
            match pfx.try_reinterpret::<fmt::UTF8>() {
                Ok(s) => {
                    debug_assert_eq!(1, s.chars().count());
                    self.sink.process(s);
                }

                Err(_) => {
                    self.sink.error(Cow::Borrowed(INVALID));
                    self.emit_char('\u{fffd}');
                }
            }
        }

        if t.len() == 0 {
            return;
        }

        let pop = match futf::classify(&*t, t.len() - 1) {
            Some(Codepoint { meaning: Meaning::Prefix(n), bytes, rewind }) => {
                self.pfx.push_slice(bytes);
                self.need = n;
                (rewind + 1) as u32
            }
            _ => 0,
        };
        if pop > 0 {
            t.pop_back(pop);
        }

        if t.len() == 0 {
            return;
        }

        match t.try_reinterpret::<fmt::UTF8>() {
            Ok(s) => self.sink.process(s),

            Err(t) => {
                // FIXME: We don't need to copy the whole chunk
                self.sink.error(Cow::Borrowed(INVALID));
                let s = t.decode(encoding::all::UTF_8, DecoderTrap::Replace).unwrap();
                self.sink.process(s);
            }
        }
    }

    #[inline]
    fn finish(&mut self) {
        if self.need > 0 {
            debug_assert!(self.pfx.len() != 0);
            self.sink.error(Cow::Borrowed("incomplete byte sequence at end of stream"));
            self.emit_char('\u{fffd}');
        }
        self.sink.finish();
    }

    #[inline]
    fn error(&mut self, desc: Cow<'static, str>) {
        self.sink.error(desc);
    }
}

/// Incrementally decode a byte stream to UTF-8.
///
/// This will write the decoded characters into new tendrils.
/// To validate UTF-8 without copying, see `UTF8Validator`
/// in this module.
pub struct Decoder<Sink> {
    decoder: Box<RawDecoder>,
    sink: Sink,
}

impl<Sink> Decoder<Sink>
    where Sink: TendrilSink<fmt::UTF8>,
{
    /// Create a new incremental decoder.
    #[inline]
    pub fn new(encoding: EncodingRef, sink: Sink) -> Decoder<Sink> {
        Decoder {
            decoder: encoding.raw_decoder(),
            sink: sink,
        }
    }

    /// Consume the decoder and obtain the sink.
    #[inline]
    pub fn into_sink(self) -> Sink {
        self.sink
    }
}

impl<Sink> TendrilSink<fmt::Bytes> for Decoder<Sink>
    where Sink: TendrilSink<fmt::UTF8>,
{
    #[inline]
    fn process(&mut self, mut t: Tendril<fmt::Bytes>) {
        let mut out = Tendril::new();
        loop {
            match self.decoder.raw_feed(&*t, &mut out) {
                (_, Some(err)) => {
                    out.push_char('\u{fffd}');
                    self.sink.error(err.cause);
                    debug_assert!(err.upto >= 0);
                    t.pop_front(err.upto as u32);
                    // continue loop and process remainder of t
                }
                (_, None) => break,
            }
        }
        if out.len() > 0 {
            self.sink.process(out);
        }
    }

    #[inline]
    fn finish(&mut self) {
        let mut out = Tendril::new();
        if let Some(err) = self.decoder.raw_finish(&mut out) {
            out.push_char('\u{fffd}');
            self.sink.error(err.cause);
        }
        if out.len() > 0 {
            self.sink.process(out);
        }
        self.sink.finish();
    }

    #[inline]
    fn error(&mut self, desc: Cow<'static, str>) {
        self.sink.error(desc);
    }
}

#[cfg(test)]
mod test {
    use super::{TendrilSink, Decoder, UTF8Validator};
    use tendril::{Tendril, SliceExt};
    use fmt;
    use std::borrow::Cow;
    use encoding::EncodingRef;
    use encoding::all as enc;

    struct Accumulate {
        tendrils: Vec<Tendril<fmt::UTF8>>,
        errors: Vec<String>,
    }

    impl Accumulate {
        fn new() -> Accumulate {
            Accumulate {
                tendrils: vec![],
                errors: vec![],
            }
        }
    }

    impl TendrilSink<fmt::UTF8> for Accumulate {
        fn process(&mut self, t: Tendril<fmt::UTF8>) {
            self.tendrils.push(t);
        }

        fn error(&mut self, desc: Cow<'static, str>) {
            self.errors.push(desc.into_owned());
        }
    }

    fn check_validate(input: &[&[u8]], expected: &[&str], errs: usize) {
        let mut validator = UTF8Validator::new(Accumulate::new());
        for x in input {
            validator.process(x.to_tendril());
        }
        validator.finish();

        let Accumulate { tendrils, errors } = validator.into_sink();
        assert_eq!(expected.len(), tendrils.len());
        for (&e, t) in expected.iter().zip(tendrils.iter()) {
            assert_eq!(e, &**t);
        }
        assert_eq!(errs, errors.len());
    }

    #[test]
    fn validate_utf8() {
        check_validate(&[], &[], 0);
        check_validate(&[b""], &[], 0);
        check_validate(&[b"xyz"], &["xyz"], 0);
        check_validate(&[b"x", b"y", b"z"], &["x", "y", "z"], 0);

        check_validate(&[b"xy\xEA\x99\xAEzw"], &["xy\u{a66e}zw"], 0);
        check_validate(&[b"xy\xEA", b"\x99\xAEzw"], &["xy", "\u{a66e}", "zw"], 0);
        check_validate(&[b"xy\xEA\x99", b"\xAEzw"], &["xy", "\u{a66e}", "zw"], 0);
        check_validate(&[b"xy\xEA", b"\x99", b"\xAEzw"], &["xy", "\u{a66e}", "zw"], 0);
        check_validate(&[b"\xEA", b"", b"\x99", b"", b"\xAE"], &["\u{a66e}"], 0);
        check_validate(&[b"", b"\xEA", b"", b"\x99", b"", b"\xAE", b""], &["\u{a66e}"], 0);

        check_validate(&[b"xy\xEA", b"\xFF", b"\x99\xAEz"],
            &["xy", "\u{fffd}", "\u{fffd}z"], 2);
        check_validate(&[b"xy\xEA\x99", b"\xFFz"],
            &["xy", "\u{fffd}", "z"], 1);

        check_validate(&[b"\xC5\x91\xC5\x91\xC5\x91"], &["őőő"], 0);
        check_validate(&[b"\xC5\x91", b"\xC5\x91", b"\xC5\x91"], &["ő", "ő", "ő"], 0);
        check_validate(&[b"\xC5", b"\x91\xC5", b"\x91\xC5", b"\x91"],
            &["ő", "ő", "ő"], 0);
        check_validate(&[b"\xC5", b"\x91\xff", b"\x91\xC5", b"\x91"],
            &["ő", "\u{fffd}", "\u{fffd}", "ő"], 2);

        // incomplete char at end of input
        check_validate(&[b"\xC0"], &["\u{fffd}"], 1);
        check_validate(&[b"\xEA\x99"], &["\u{fffd}"], 1);
    }

    fn check_decode(enc: EncodingRef, input: &[&[u8]], expected: &str, errs: usize) {
        let mut decoder = Decoder::new(enc, Accumulate::new());
        for x in input {
            decoder.process(x.to_tendril());
        }
        decoder.finish();

        let Accumulate { tendrils, errors } = decoder.into_sink();
        let mut tendril: Tendril<fmt::UTF8> = Tendril::new();
        for t in tendrils {
            tendril.push_tendril(&t);
        }
        assert_eq!(expected, &*tendril);
        assert_eq!(errs, errors.len());
    }

    #[test]
    fn decode_ascii() {
        check_decode(enc::ASCII, &[], "", 0);
        check_decode(enc::ASCII, &[b""], "", 0);
        check_decode(enc::ASCII, &[b"xyz"], "xyz", 0);
        check_decode(enc::ASCII, &[b"xy", b"", b"", b"z"], "xyz", 0);
        check_decode(enc::ASCII, &[b"x", b"y", b"z"], "xyz", 0);

        check_decode(enc::ASCII, &[b"\xFF"], "\u{fffd}", 1);
        check_decode(enc::ASCII, &[b"x\xC0yz"], "x\u{fffd}yz", 1);
        check_decode(enc::ASCII, &[b"x", b"\xC0y", b"z"], "x\u{fffd}yz", 1);
        check_decode(enc::ASCII, &[b"x\xC0yz\xFF\xFFw"], "x\u{fffd}yz\u{fffd}\u{fffd}w", 3);
    }

    #[test]
    fn decode_utf8() {
        check_decode(enc::UTF_8, &[], "", 0);
        check_decode(enc::UTF_8, &[b""], "", 0);
        check_decode(enc::UTF_8, &[b"xyz"], "xyz", 0);
        check_decode(enc::UTF_8, &[b"x", b"y", b"z"], "xyz", 0);

        check_decode(enc::UTF_8, &[b"\xEA\x99\xAE"], "\u{a66e}", 0);
        check_decode(enc::UTF_8, &[b"\xEA", b"\x99\xAE"], "\u{a66e}", 0);
        check_decode(enc::UTF_8, &[b"\xEA\x99", b"\xAE"], "\u{a66e}", 0);
        check_decode(enc::UTF_8, &[b"\xEA", b"\x99", b"\xAE"], "\u{a66e}", 0);
        check_decode(enc::UTF_8, &[b"\xEA", b"", b"\x99", b"", b"\xAE"], "\u{a66e}", 0);
        check_decode(enc::UTF_8, &[b"", b"\xEA", b"", b"\x99", b"", b"\xAE", b""], "\u{a66e}", 0);

        check_decode(enc::UTF_8, &[b"xy\xEA", b"\x99\xAEz"], "xy\u{a66e}z", 0);
        check_decode(enc::UTF_8, &[b"xy\xEA", b"\xFF", b"\x99\xAEz"],
            "xy\u{fffd}\u{fffd}\u{fffd}\u{fffd}z", 4);
        check_decode(enc::UTF_8, &[b"xy\xEA\x99", b"\xFFz"],
            "xy\u{fffd}\u{fffd}z", 2);

        // incomplete char at end of input
        check_decode(enc::UTF_8, &[b"\xC0"], "\u{fffd}", 1);
        check_decode(enc::UTF_8, &[b"\xEA\x99"], "\u{fffd}", 1);
    }

    #[test]
    fn decode_koi8_u() {
        check_decode(enc::KOI8_U, &[b"\xfc\xce\xc5\xd2\xc7\xc9\xd1"], "Энергия", 0);
        check_decode(enc::KOI8_U, &[b"\xfc\xce", b"\xc5\xd2\xc7\xc9\xd1"], "Энергия", 0);
        check_decode(enc::KOI8_U, &[b"\xfc\xce", b"\xc5\xd2\xc7", b"\xc9\xd1"], "Энергия", 0);
        check_decode(enc::KOI8_U, &[b"\xfc\xce", b"", b"\xc5\xd2\xc7", b"\xc9\xd1", b""], "Энергия", 0);
    }

    #[test]
    fn decode_windows_949() {
        check_decode(enc::WINDOWS_949, &[], "", 0);
        check_decode(enc::WINDOWS_949, &[b""], "", 0);
        check_decode(enc::WINDOWS_949, &[b"\xbe\xc8\xb3\xe7"], "안녕", 0);
        check_decode(enc::WINDOWS_949, &[b"\xbe", b"\xc8\xb3\xe7"], "안녕", 0);
        check_decode(enc::WINDOWS_949, &[b"\xbe", b"", b"\xc8\xb3\xe7"], "안녕", 0);
        check_decode(enc::WINDOWS_949, &[b"\xbe\xc8\xb3\xe7\xc7\xcf\xbc\xbc\xbf\xe4"],
            "안녕하세요", 0);
        check_decode(enc::WINDOWS_949, &[b"\xbe\xc8\xb3\xe7\xc7"], "안녕\u{fffd}", 1);

        check_decode(enc::WINDOWS_949, &[b"\xbe", b"", b"\xc8\xb3"], "안\u{fffd}", 1);
        check_decode(enc::WINDOWS_949, &[b"\xbe\x28\xb3\xe7"], "\u{fffd}(녕", 1);
    }
}
