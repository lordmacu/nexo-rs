.PHONY: dev build test lint fmt check clean docker-up docker-down docker-logs integration-smoke integration-browser integration-recovery integration-suite extensions-smoke setup setup-wizard setup-list setup-doctor setup-google setup-google-docker

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

integration-smoke:
	./scripts/integration_stack_smoke.sh

integration-browser:
	cargo run --quiet --bin integration-browser-check

integration-recovery:
	./scripts/integration_nats_recovery.sh

integration-suite:
	docker compose up -d
	./scripts/integration_stack_smoke.sh
	./scripts/extensions_smoke.sh
	cargo run --quiet --bin integration-browser-check
	./scripts/integration_nats_recovery.sh

extensions-smoke:
	./scripts/extensions_smoke.sh

CONFIG_DIR ?= config/docker

setup:
	cp -n .env.example .env || true
	mkdir -p data secrets logs
	@echo "Edit .env with your API keys, then run: make setup-wizard"

# Interactive menu — shows every service the wizard knows about.
setup-wizard:
	cargo run --quiet --bin agent -- --config $(CONFIG_DIR) setup

# List all services (ids + labels) that setup knows.
setup-list:
	cargo run --quiet --bin agent -- --config $(CONFIG_DIR) setup list

# Audit: which services are configured, which are missing secrets.
setup-doctor:
	cargo run --quiet --bin agent -- --config $(CONFIG_DIR) setup doctor

# Jump straight to Google OAuth (client_id + client_secret prompt).
setup-google:
	cargo run --quiet --bin agent -- --config $(CONFIG_DIR) setup google-auth

# Same, but inside the agent Docker image — useful when building the
# release image is already done and cargo is not available on the host.
# Mounts secrets/ and config/ as rw so the wizard can persist.
setup-google-docker:
	docker compose run --rm \
	  -v $(PWD)/config:/app/config \
	  -v $(PWD)/secrets:/app/config/secrets \
	  --entrypoint agent \
	  agent --config /app/$(CONFIG_DIR) setup google-auth
