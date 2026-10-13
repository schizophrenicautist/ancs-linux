.PHONY: build test lint install uninstall

build:
	cargo build --release --locked

test:
	cargo test --locked

lint:
	cargo clippy --all-targets --locked -- -D warnings

install:
	./install.sh --from-source

uninstall:
	./install.sh --uninstall
