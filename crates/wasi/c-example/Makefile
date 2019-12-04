ROOT_DIR=../../..
run-cube: run-cube.c
	clang \
		-I$(ROOT_DIR)/crates/api/c-examples/wasm-c-api/include -L$(ROOT_DIR)/target/debug \
		-lwasmtime -lwasmtime_wasi \
		run-cube.c -o run-cube

run: run-cube
	./run-cube

.PHONY: run
