.PHONY: test daemon web dev

CTX_DAEMON_BIND ?= 127.0.0.1:4399
CTX_DATA_DIR ?= $(HOME)/.ctx
CTX_WEB_PORT ?= 5173

test:
	cargo test --workspace
	pnpm -C apps/web test

daemon:
	cargo run -p ctx-http --bin ctx -- serve --bind $(CTX_DAEMON_BIND) --data-dir $(CTX_DATA_DIR)

web:
	CTX_WEB_PORT=$(CTX_WEB_PORT) pnpm -C apps/web dev

dev:
	@echo "Start the daemon and web workbench in separate shells:"
	@echo "  make daemon"
	@echo "  make web"
