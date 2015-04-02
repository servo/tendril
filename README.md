# tendril

The code in this repo is **not ready for use**! For now it's just a place to
discuss the project. Please open GitHub issues or find me on IRC if you have
thoughts about the proposal.

## Introduction

[html5ever][]'s API works mostly in terms of owned strings. The API consumer
passes owned strings into the parser, and receives text content in the form of
owned strings.

Originally these APIs all used `String`, but they have become more elaborate
over time. For strings that need fast comparison, we have an [interning
system][] that is [working very well][]. Non-interned strings (mainly text
nodes and attribute values) used `String` until the recent zero-copy parsing
work ([PR #60][], [PR #114][]).

`Tendril` is an owned, non-interned string type for html5ever, that will
eventually replace both `String` and `IOBuf` in this capacity. It could also
find use in other Rust programs dealing with either text or binary data.

## Implemented

### Zero-copy parsing

We should represent strings as slices of the source document, when practical.
But we want to keep the semantics of owned strings; we don't want to add
lifetime parameters.

*Solution:* **thread-local refcounting**, which adds minimal overhead.

### Cheap in-place append

When a string's refcount is 1, meaning we have unique ownership, we should
support appending data in-place.

*Solution:* Use a **growable buffer**, like `String` does. Reallocate only if
we exceed the capacity, and try to do the reallocation in place. Use **pointer
tagging** so we don't have to check the refcount on every append.

### Support for multiple encodings

html5ever uses UTF-8 internally. But we must handle [incomplete character
reads](https://github.com/servo/html5ever/issues/34), [conversion from other
encodings](https://github.com/servo/html5ever/issues/18), and UCS-2
[`document.write`](https://github.com/servo/html5ever/issues/6).

*Solution:* Use phantom types to **track a buffer's encoding statically**.
Only UTF-8 provides `Deref<Target=str>`. **Support [WTF-8][]** and provide
[zero-copy conversion](http://simonsapin.github.io/wtf-8/#converting-wtf-8-utf-8)
to UTF-8 after checking for lone surrogates.

This library will integrate with rust-encoding's incremental conversion
support.  Converting to/from UTF-8 may be one of the *only* operations
supported on non-UTF-8 buffers.

To make the library usable for non-textual data, we also have a `Bytes`
encoding, which provides `Deref<Target=[u8]>` and other APIs to mirror
`Vec<u8>`.

### Compact representation

On a 64-bit platform, `String` is 24 bytes, which is a lot to pass by value.
Moving strings should be as cheap as possible.

*Solution:* **use `u32` for length and capacity**, following `IOBuf`'s example.
This shrinks the structure to 16 bytes (12 bytes on 32-bit). It also **limits
strings to 4 GB**, but see below.

As a streaming parser, html5ever is free to impose limits on how much data it
will process at once. These can be satisfied regardless of the size of elements
in the input document.

### Small string optimization

Most of the strings produced by html5ever are either long spans from the source
document, or they are very short — usually, a single ASCII character that
terminated a tokenizer fast path. We should avoid heap allocations for these
short strings.

*Solution:* **store short strings directly inside** that 16-byte structure.

## Not implemented yet

### C compatible

Clients of the html5ever C API should enjoy the same zero-copy parsing
performance. (*work in progress*)

*Solution:* Provide **a C API** for creating and using these refcounted strings.
Conversion to (pointer, length) is provided as an `inline` function in a C
header file.

### Ropes

It would be nice to preserve the zero-copy UTF-8 representation all the way
through to Servo's DOM. This would reduce memory consumption, and possibly
speed up text shaping and painting. However, DOM text may conceivably be larger
than 4 GB, and will anyway not be contiguous in memory around e.g. a character
entity reference.

*Solution:* Build a **[rope][] on top of these strings** and use that as
Servo's representation of DOM text. We can perhaps do text shaping and/or
painting in parallel for different chunks of a rope. html5ever can additionally
use this rope type as a replacement for `BufferQueue`.

Because the underlying buffers are reference-counted, the bulk of this rope
is already a [persistent data structure][]. Consider what happens when
appending two ropes to get a "new" rope. A vector-backed rope would copy a
vector of small structs, one for each chunk, and would bump the corresponding
refcounts. But it would not copy any of the string data.

If we want more sharing, then a [2-3 finger tree][] could be a good choice.
We would probably stick with `VecDeque` for ropes under a certain size.

### UTF-16 compatible

SpiderMonkey expects text to be in UCS-2 format for the most part. The
semantics of JavaScript strings are difficult to implement on UTF-8. Also,
passing SpiderMonkey a string that isn't contiguous in memory will incur
additional overhead and complexity, if not a full copy.

*Solution:* Servo will **convert to contiguous UTF-16 when necessary**.  The
conversion can easily be parallelized, if we find a practical need to convert
huge chunks of text all at once.

### Sendable

We don't need to share strings between threads, but we do need to move them.

*Solution:* Provide a **separate type for sendable strings**. Converting to
this type entails a copy, unless the refcount is 1.

### Optional atomic refcounting

The above `Send` implementation is not good enough for off-main-thread parsing
in Servo. We will end up copying every small string when we send it to the main
thread.

*Solution:* Use another phantom type to **designate strings which are
atomically refcounted**. You "set" this type variable when you create a string
or promote one from uniquely owned. This statically eliminates the overhead of
atomic refcounting for consumers who don't need strings to have guaranteed
zero-copy `Send`. html5ever will be generic over this choice.

### Source span information

Some html5ever API consumers want to know the originating location in the HTML
source file(s) of each token or parse error. An example application would be a
command-line HTML validator with diagnostic output similar to `rustc`'s.

*Solution:* Accept **some metadata along with each input string**. The type of
metadata is chosen by the API consumer; it defaults to `()`, which has size
zero. For any non-inline string, we can provide the associated metadata as well
as a byte offset.

## Representation details

A `Tendril` comprises one `usize` field and two `u32` fields. There are four
forms:

form   | `ptr: usize`      | `len: u32`      | `aux: u32`
-------|-------------------|-----------------|-----------
inline | `0000` ... `nnnn` | 1≤n≤8 bytes     | cont'd ...
empty  | `0000` ... `1111` | undefined       | undefined
owned  | `hhhh` ... `hhh0` | length          | capacity
shared | `hhhh` ... `hhh1` | length          | offset

The `hhhh...` bits point to a fixed-size header, allocated just before the
string contents. It should be clear that for each of these forms, we can build
a (pointer, length) pair (i.e. a slice) without any additional memory access.

The obvious representation for an empty string is `ptr == 0`, but we avoid this
to take advantage of the [`NonZero` optimization][NonZero]. This means that
`Option<Tendril>` takes the same amount of space as `Tendril`.

The header on a non-inline string has a refcount and a capacity field, along
with any metadata specified by the API consumer. However, an owned string
stores its capacity in-line. This means we can append at the end of the string
without touching the header at all. The header's capacity field is unused and
undefined until we clone the owned string, forming a shared string. At that
point the capacity is saved in the header for later use in deallocation, or in
conversion back to an owned string.

When we clone an owned string, we also set the LSB of the original pointer to
`1` and copy 0 into the `aux` field. This is accomplished using `Cell`.

It is possible that bit 1 of `ptr` is set even if the string has refcount 1.
This will happen when slicing in place with a nonzero starting index. It's the
price we pay for having a 16-byte structure; we can't fit both capacity and
offset.

[NonZero]: http://doc.rust-lang.org/core/nonzero/struct.NonZero.html
[html5ever]: https://github.com/servo/html5ever
[interning system]: https://github.com/servo/string-cache
[working very well]: https://github.com/servo/servo/wiki/Meeting-2014-10-27#string-cache-and-h5ever-performance-update
[PR #60]: https://github.com/servo/html5ever/pull/60
[PR #114]: https://github.com/servo/html5ever/pull/114
[WTF-8]: http://simonsapin.github.io/wtf-8/
[rope]: http://en.wikipedia.org/wiki/Rope_%28data_structure%29
[persistent data structure]: http://en.wikipedia.org/wiki/Persistent_data_structure
[2-3 finger tree]: http://staff.city.ac.uk/~ross/papers/FingerTree.html
