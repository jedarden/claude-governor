.PHONY: build install clean test verify-release

PREFIX ?= $(HOME)/.local

# Target used for the static release binary. CGOV_CI builds it for
# x86_64-unknown-linux-musl; point MUSL_TARGET elsewhere to validate a
# different triple (e.g. aarch64-unknown-linux-musl — the execution probe
# runs under a qemu-user emulator when one is installed, and is skipped
# with a loud note when not). cgov-ci installs qemu-user-static and fails
# closed if the foreign probe does not report both PASS lines.
MUSL_TARGET ?= x86_64-unknown-linux-musl

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

# Enforce the README's zero-runtime-dependency promise: build the static
# release binary, then require no dynamic loader, no shared libraries, and a
# clean run in a scrubbed environment (see scripts/verify-release-static.sh).
# On hosts without a musl C compiler on PATH, point cc-rs at one for the
# bundled SQLite, e.g. on codinghome:
#   CC_x86_64_unknown_linux_musl=$$(nix-shell -p musl --run 'command -v musl-gcc')
verify-release:
	cargo build --release --target $(MUSL_TARGET)
	scripts/verify-release-static.sh
