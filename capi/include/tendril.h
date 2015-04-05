#ifndef _TENDRIL_H
#define _TENDRIL_H

#include <stdint.h>
#include <stdarg.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

#define TENDRIL_EMPTY_TAG 0xF
#define TENDRIL_INIT { TENDRIL_EMPTY_TAG, 0, 0 }

// private
#define TENDRIL_MAX_INLINE_TAG 0xF
#define TENDRIL_HEADER_LEN (sizeof(char *) + 4)

typedef struct __attribute__((packed)) tendril_impl {
    uintptr_t _ptr;
    uint32_t _a;
    uint32_t _b;
} tendril;

static inline char *tendril_data(const tendril *t) {
    uintptr_t p = t->_ptr;
    if (p <= TENDRIL_MAX_INLINE_TAG) {
        return (char *) &t->_a;
    } else {
        return (char *) ((p & ~1) + TENDRIL_HEADER_LEN);
    }
}

static inline uint32_t tendril_len(const tendril *t) {
    uintptr_t p = t->_ptr;
    if (p == TENDRIL_EMPTY_TAG) {
        return 0;
    } else if (p <= TENDRIL_MAX_INLINE_TAG) {
        return p;
    } else {
        return t->_a;
    }
}

// Defined in src/lib.rs

void tendril_clone(tendril *t, const tendril *r);
void tendril_sub(tendril *t,
                 const tendril *r,
                 uint32_t offset,
                 uint32_t length);

void tendril_destroy(tendril *t);

void tendril_clear(tendril *t);
void tendril_push_buffer(tendril *t, const char *buffer, uint32_t length);
void tendril_push_string(tendril *t, const char *str);
void tendril_push_tendril(tendril *t, const tendril *r);
void tendril_push_uninit(tendril *t, uint32_t n);
void tendril_pop_front(tendril *t, uint32_t n);
void tendril_pop_back(tendril *t, uint32_t n);
void tendril_debug_describe(tendril *desc, const tendril *t);

// Defined in src/glue.c

int tendril_vsprintf(tendril *t, const char *format, va_list ap);
int tendril_sprintf(tendril *t, const char *format, ...);

void tendril_debug_dump(const tendril *t, FILE *stream);
size_t tendril_fwrite(const tendril *t, FILE *stream);

#undef TENDRIL_MAX_INLINE_TAG
#undef TENDRIL_HEADER_LEN

#ifdef __cplusplus
}
#endif

#endif
