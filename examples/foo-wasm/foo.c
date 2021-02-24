// ~/bin/wasi-sdk/bin/clang foo.c -o foo.wasm -Wl,--no-entry,--export-all,--export-table,--growable-table -nostdlib -lc
// ~/bin/wabt/wasm2wat foo.wasm -o foo.wat

#include <stdio.h>

typedef int (*callback)(int i);

int bar(int i, callback f) {
  int j = f(i);
  fprintf(stderr, "test %d->%d\n", i, j);
  return j;
}
