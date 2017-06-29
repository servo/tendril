// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#ifndef _TENDRIL_H
#define _TENDRIL_H

#include <stdint.h>
#include <stdarg.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

// C equivalent of `ByteTendril`.
//
// See https://kmcallister.github.io/docs/tendril/tendril/struct.Tendril.html
//
// This is a small structure, probably 12 or 16 bytes. You can allocate it
// anywhere you like, but you *must* initialize it with `TENDRIL_INIT`.
//
// Functions that replace the content of the tendril will take care of
// deallocating any storage that becomes unused. The closest to an explicit
// "free" is tendril_destroy, but this leaves the target in a valid state:
// an empty tendril, which does not own a heap allocation.
//
// This API does not pass `tendril` by value at any point. If your code does
// this, you should interpret it as a transfer of ownership, and refrain from
// using the source value afterwards. See also `tendril_clone`.
//
// *Warning*: It is not safe to send or share tendrils between threads!
typedef struct tendril_impl tendril;

// Initializer expression for a tendril.
//
// It is *never* safe to pass an uninitialized tendril to one of these functions.
#define TENDRIL_INIT { 0xF, 0, 0 }

// Get a pointer to the data in a tendril.
static inline char *tendril_data(const tendril *t);

// Get the number of bytes stored in a tendril.
static inline uint32_t tendril_len(const tendril *t);

// Replace `t` with a copy of `r`.
//
// This will share the backing storage when practical.
void tendril_clone(tendril *t, const tendril *r);

// Replace `t` with a slice of `r`.
//
// This will share the backing storage when practical.
void tendril_sub(tendril *t, const tendril *r, uint32_t offset, uint32_t length);

// Deallocate any storage associated with the tendril, and replace it with
// an empty tendril (which does not own a heap allocation).
void tendril_destroy(tendril *t);

// Truncate to length 0 *without* discarding any owned storage.
void tendril_clear(tendril *t);

// Push some bytes onto the back of the tendril.
void tendril_push_buffer(tendril *t, const char *buffer, uint32_t length);

// Push another tendril onto the back.
void tendril_push_tendril(tendril *t, const tendril *r);

// Push "uninitialized bytes" onto the back.
//
// Really, this grows the tendril without writing anything to the new area.
void tendril_push_uninit(tendril *t, uint32_t n);

// Remove bytes from the front.
void tendril_pop_front(tendril *t, uint32_t n);

// Remove bytes from the back.
void tendril_pop_back(tendril *t, uint32_t n);

// Replace `desc` with a tendril that describes (in ASCII text) the tendril
// `t`, including some details of how it is stored.
void tendril_debug_describe(tendril *desc, const tendril *t);

// Push text onto the back of a tendril according to a format string.
//
// This does *not* push a NULL terminator.
int tendril_sprintf(tendril *t, const char *format, ...);

// See tendril_sprintf.
int tendril_vsprintf(tendril *t, const char *format, va_list ap);

// Write the bytes of the tendril to a stdio stream.
size_t tendril_fwrite(const tendril *t, FILE *stream);

// Write a description in ASCII text of the tendril `t`, including some
// details of how it is stored.
void tendril_debug_dump(const tendril *t, FILE *stream);

////
//// implementation details follow
////

struct tendril_impl {
    uintptr_t __ptr;
    uint32_t __a;
    uint32_t __b;
};

#define __TENDRIL_EMPTY_TAG 0xF
#define __TENDRIL_MAX_INLINE_TAG 0xF
#define __TENDRIL_HEADER_LEN (sizeof(char *) + 4)

static inline char *tendril_data(const tendril *t) {
    uintptr_t p = t->__ptr;
    if (p <= __TENDRIL_MAX_INLINE_TAG) {
        return (char *) &t->__a;
    } else {
        return (char *) ((p & ~1) + __TENDRIL_HEADER_LEN);
    }
}

static inline uint32_t tendril_len(const tendril *t) {
    uintptr_t p = t->__ptr;
    if (p == __TENDRIL_EMPTY_TAG) {
        return 0;
    } else if (p <= __TENDRIL_MAX_INLINE_TAG) {
        return p;
    } else {
        return t->__a;
    }
}

#undef __TENDRIL_EMPTY_TAG
#undef __TENDRIL_MAX_INLINE_TAG
#undef __TENDRIL_HEADER_LEN

#ifdef __cplusplus
}
#endif

#endif
