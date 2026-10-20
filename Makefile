.PHONY: run dev build package zip store-update install update check-update list-updates verify test clean

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

store-update:
	./scripts/package-macos.sh --store-local

install:
	./scripts/install.sh

update:
	./scripts/update.sh

check-update:
	./scripts/update.sh --check

list-updates:
	./scripts/update.sh --list

verify:
	cargo run -p spot-verify

test:
	cargo test --workspace
	cargo run -p spot-verify

clean:
	cargo clean
	rm -rf dist
