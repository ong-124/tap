install:
	cargo install cargo-server
	cargo install wasm-bindgen-cli
	cargo install cargo-watch
	# Binaryen is also needed
	# https://github.com/WebAssembly/binaryen

run: build opt
	cargo server --open

opt:
	du -h ./wasm/tapterm_bg.wasm
	wasm-opt -O ./wasm/tapterm_bg.wasm -o ./wasm/tapterm_bg.wasm
	du -h ./wasm/tapterm_bg.wasm

watch:
	cargo watch -- make build

build:
	wasm-bindgen ../target/wasm32-unknown-unknown/debug/tapterm.wasm --out-dir wasm --target web --no-typescript

build-release:
	wasm-bindgen ../target/wasm32-unknown-unknown/release/tapterm.wasm --out-dir wasm --target web --no-typescript