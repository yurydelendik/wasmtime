#!/bin/bash

CLANG=~/bin/wasi-sdk/bin/clang

$CLANG foo.c -o foo.wasm -Wl,--no-entry,--export-all,--export-table,--growable-table -nostdlib -lc
# wasm2wat foo.wasm -o foo.wat
cargo run -- wasm2obj foo.wasm foo.o
ar rcs libfoo.a foo.o
