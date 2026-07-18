.PHONY: build check install-local lint test

build:
	cargo build --release

check:
	cargo check

lint:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test --all-targets

LOCAL_PREFIX ?= $(HOME)/.local

install-local:
	cargo install --locked --path . --root "$(LOCAL_PREFIX)" --force

