.PHONY: dev build test lint fmt check clean docker-up docker-down docker-logs

dev:
	RUST_LOG=debug cargo run --bin agent -- --config config/agents.yaml

build:
	cargo build --workspace

release:
	cargo build --workspace --release

test:
	cargo test --workspace

lint:
	cargo clippy --workspace -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

check: fmt-check lint test

clean:
	cargo clean

docker-up:
	docker compose up -d

docker-down:
	docker compose down

docker-logs:
	docker compose logs -f agent

docker-build:
	docker compose build

setup:
	cp .env.example .env
	mkdir -p data secrets logs
	@echo "Edit .env with your API keys, then run: make dev"
