// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#include <errno.h>
#include <stdio.h>
#include <stdarg.h>

#include "tendril.h"

int tendril_vsprintf(tendril *t, const char *format, va_list args) {
    // This is a lot like asprintf.
    va_list args_copy;
    va_copy(args_copy, args);

    int ret = vsnprintf(NULL, 0, format, args);

    if (ret > 0xFFFFFFFF) {
        errno = E2BIG;
        ret = -1;
    } else if (ret >= 0) {
        uint32_t addnl = ret + 1; // include null terminator
        uint32_t old_len = tendril_len(t);
        tendril_push_uninit(t, addnl);

        ret = vsnprintf(tendril_data(t) + old_len, addnl, format, args_copy);

        // Pop the NULL terminator.
        tendril_pop_back(t, 1);
    }

    va_end(args_copy);
    return ret;
}

int tendril_sprintf(tendril *t, const char *format, ...) {
    va_list args;
    va_start(args, format);
    int ret = tendril_vsprintf(t, format, args);
    va_end(args);
    return ret;
}

void tendril_debug_dump(const tendril *t, FILE *stream) {
    tendril dbg = TENDRIL_INIT;
    tendril_debug_describe(&dbg, t);
    tendril_fwrite(&dbg, stream);
    tendril_destroy(&dbg);
}

size_t tendril_fwrite(const tendril *t, FILE *stream) {
    return fwrite(tendril_data(t), 1, tendril_len(t), stream);
}
