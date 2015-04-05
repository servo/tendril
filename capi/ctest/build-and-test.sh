#!/bin/sh

set -xe

(cd ..; cargo build)
cp ../target/debug/libtendril_capi-*.a ./libtendril_capi.a
gcc -o test test.c -Wall -I ../include -L . -ltendril_capi -ldl -lpthread -lrt -lgcc_s -lpthread -lc -lm
./test > out.actual
diff -u out.expect out.actual
