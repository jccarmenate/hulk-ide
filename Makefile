.PHONY: build

build:
	cargo build --release
	cp target/release/hulk-cli ./hulk
