# Build/package entry points. CI uses native runners; no device operations here.
VERSION := $(shell cat packaging/VERSION)
CRATE_VERSION := $(shell sed -n '/^\[package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
DOCKER ?= docker
BUILD_IMAGE ?= rust:1.99-alpine
TARGET_DIR ?= target/alpine
APK_DIR ?= target/apk
BLUEZ_RUNTIME ?= target/bluez-runtime
LINUX_ARCH ?= arm64
LINUX_BINARY ?= target/linux/release/cowplay
LINUX_TAR ?= target/linux-dist/cowplay-$(VERSION)-linux-$(LINUX_ARCH).tar.gz
CORE_APK := $(APK_DIR)/cowplay-$(VERSION).apk
LUCI_APK := $(APK_DIR)/luci-app-cowplay-$(VERSION).apk
CONTAINER := $(DOCKER) run --rm --platform linux/arm64 -v "$(CURDIR)":/src -w /src \
	-v cowplay-cargo-registry:/usr/local/cargo/registry \
	-e CARGO_TARGET_DIR=/src/$(TARGET_DIR) $(BUILD_IMAGE) sh -ec

.DEFAULT_GOAL := help
.PHONY: help deps check-version web build apk linux-tar test test-rust test-web test-package test-apk
help:
	@echo 'Package version: $(VERSION)'
	@echo '  make deps         install locked frontend/Rust dependencies'
	@echo '  make build        build web UI and ARM64 core/noarch LuCI APKs'
	@echo '  make apk          package ARM64 core/LuCI (TARGET_DIR selects the Rust build cache)'
	@echo '  make linux-tar    package LINUX_BINARY for LINUX_ARCH=arm64 or x86_64'
	@echo '  make test         Rust, frontend logic and package lifecycle tests'
	@echo '  make test-apk     install/remove actual double APKs in a disposable OpenWrt root'

deps:
	npm ci --prefix web
	cargo fetch --locked

check-version:
	@version='$(VERSION)'; revision=$${version#"$(CRATE_VERSION)-r"}; \
		case "$$revision" in ''|*[!0-9]*) echo 'invalid package version: $(VERSION)' >&2; exit 1;; esac

web:
	npm run build --prefix web

build: web apk

$(BLUEZ_RUNTIME)/SHA256SUMS:
	mkdir -p $(dir $(BLUEZ_RUNTIME))
	$(CONTAINER) 'apk add --no-cache curl >/dev/null; sh scripts/prepare-bluez.sh $(BLUEZ_RUNTIME)'

apk: check-version $(BLUEZ_RUNTIME)/SHA256SUMS
	mkdir -p $(APK_DIR)
	$(CONTAINER) 'apk add --no-cache musl-dev >/dev/null; \
		cargo build --locked --release; \
		sh scripts/build-apk.sh $(TARGET_DIR)/release/cowplay $(CORE_APK) $(BLUEZ_RUNTIME); \
		sh scripts/build-luci-apk.sh $(LUCI_APK)'

linux-tar: check-version
	python3 scripts/build-linux-tar.py "$(LINUX_BINARY)" "$(LINUX_ARCH)" "$(LINUX_TAR)"

test: test-rust test-web test-package

test-rust:
	cargo test --workspace --locked

test-web:
	node scripts/test-frame-progress.mjs
	node scripts/test-viewer-ping.mjs
	node scripts/test-video-resolution.mjs

test-package:
	mkdir -p target
	python3 scripts/test-package-lifecycle.py
	python3 scripts/test-sysupgrade-restore.py
	python3 scripts/test-linux-install.py
	python3 scripts/test-linux-download.py

test-apk:
	@test -f $(CORE_APK) && test -f $(LUCI_APK)
	$(CONTAINER) 'apk add --no-cache curl openssl python3 >/dev/null; \
		python3 scripts/test-release-artifacts.py; \
		root=$$(mktemp -d target/apk-install.XXXXXX); rmdir "$$root"; \
		sh scripts/test-luci-apk.sh $(CORE_APK) $(LUCI_APK) "$$root"'
