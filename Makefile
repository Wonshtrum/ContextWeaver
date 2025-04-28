all: output.wat

test.wasm: test.wat
	wat2wasm --debug-names $^ -o $@

output.wasm: test.wasm
	RUST_BACKTRACE=1 cargo run $^

output.wat: output.wasm
	wasm2wat $^ > $@

.PHONY: clean
clean:
	rm -f test.wasm
	rm -f output.wat
	rm -f output.wasm
