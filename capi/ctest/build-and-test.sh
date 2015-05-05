#!/bin/sh

set -xe

(cd ..; cargo build)
gcc -o test test.c -Wall -I ../include -L ../target/debug -ltendril_capi -ldl -lpthread -lrt -lgcc_s -lpthread -lc -lm
./test > out.actual
diff -u out.expect out.actual
