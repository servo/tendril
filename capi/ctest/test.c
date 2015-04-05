#include <stdio.h>

#include "tendril.h"

int main() {
    tendril t = TENDRIL_INIT;
    tendril_sprintf(&t, "Hello, %d!\n", 2015);
    tendril_fwrite(&t, stdout);

    tendril_debug_dump(&t, stdout);
    puts("");

    tendril s = TENDRIL_INIT;
    tendril_sub(&s, &t, 0, 9);
    tendril_pop_back(&s, 4);
    tendril_debug_dump(&s, stdout);
    puts("");
    tendril_debug_dump(&t, stdout);
    puts("");

    tendril_sprintf(&t, "Appending\n");
    tendril_fwrite(&s, stdout);
    tendril_fwrite(&t, stdout);

    tendril_destroy(&s);
    tendril_destroy(&t);
    return 0;
}
