.PHONY: build install clean test

PREFIX ?= $(HOME)/.local

build:
	cargo build --release

install: build
	@mkdir -p $(PREFIX)/bin
	@cp target/release/cgov $(PREFIX)/bin/cgov
	@echo "Installed cgov to $(PREFIX)/bin/cgov"

clean:
	cargo clean

test:
	cargo test
