# spm — common developer tasks.
# `make check` mirrors the CI gate (fmt + clippy + test).

CARGO   ?= cargo
PREFIX  ?= /usr/local
BINDIR  ?= $(PREFIX)/bin

.DEFAULT_GOAL := build
.PHONY: build release test fmt fmt-check lint check run install uninstall clean

## build:      debug build
build:
	$(CARGO) build

## release:    optimized (LTO, stripped) build
release:
	$(CARGO) build --release

## test:       run the integration suite
test:
	$(CARGO) test --all

## fmt:        format the code
fmt:
	$(CARGO) fmt --all

## fmt-check:  fail if code is not formatted
fmt-check:
	$(CARGO) fmt --all --check

## lint:       clippy with warnings as errors
lint:
	$(CARGO) clippy --all-targets -- -D warnings

## check:      full CI gate — fmt-check + lint + test
check: fmt-check lint test

## run:        run spm, e.g. `make run ARGS="list"`
run:
	$(CARGO) run -- $(ARGS)

## install:    build release and copy binary to $(BINDIR)
install: release
	install -d $(BINDIR)
	install -m 0755 target/release/spm $(BINDIR)/spm

## uninstall:  remove the installed binary
uninstall:
	rm -f $(BINDIR)/spm

## clean:      remove build artifacts
clean:
	$(CARGO) clean

## help:       list targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/## //'
