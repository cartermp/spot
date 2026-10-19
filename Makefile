.PHONY: run dev build package zip install verify test clean

run:
	cargo run --release -p spot-app

dev:
	cargo run -p spot-app

build:
	cargo build --release -p spot-app

package:
	./scripts/package-macos.sh

zip:
	./scripts/package-macos.sh --zip

install:
	./scripts/install.sh

verify:
	cargo run -p spot-verify

test:
	cargo test --workspace
	cargo run -p spot-verify

clean:
	cargo clean
	rm -rf dist
