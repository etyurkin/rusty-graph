CARGO ?= cargo
RELEASE_BIN := target/release/rusty-graph
DEBUG_BIN := target/debug/rusty-graph

.PHONY: all build release test install clean check fmt lint ci help run

all: release

build:
	$(CARGO) build

release:
	$(CARGO) build --release

test:
	$(CARGO) test --all

install: release
	$(CARGO) install --path .

clean:
	$(CARGO) clean

check:
	$(CARGO) check --all-targets

fmt:
	$(CARGO) fmt --all

lint:
	$(CARGO) clippy --all-targets -- -D warnings

ci: lint test

run: release
	$(RELEASE_BIN) --help

help:
	@echo "rusty-graph — local code knowledge graph for AI coding agents"
	@echo ""
	@echo "Build:"
	@echo "  all      build release binary (default)"
	@echo "  build    debug build -> $(DEBUG_BIN)"
	@echo "  release  optimized build -> $(RELEASE_BIN)"
	@echo "  clean    remove target/"
	@echo ""
	@echo "Test & check:"
	@echo "  test     run all tests"
	@echo "  check    cargo check"
	@echo "  lint     clippy with warnings denied"
	@echo "  fmt      rustfmt all sources"
	@echo "  ci       lint + test (matches GitHub Actions)"
	@echo ""
	@echo "Install:"
	@echo "  install  install rusty-graph to \$$HOME/.cargo/bin"
	@echo ""
	@echo "Other:"
	@echo "  run      show CLI help"
	@echo "  help     this message"
