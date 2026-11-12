# CowPlay developer entry points. Run from the repository root; `make help` lists targets.
# AArch64 builds and APK checks run in an arm64 Alpine container (Docker or OrbStack).
# deploy changes the F50: run it only when the current task authorizes installing there.

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
DEVICE ?= root@192.0.2.1
DEVICE_HTTP ?= http://192.0.2.1
# Needed only by test-luci (official LuCI JS runtime; see docs/UI-2026-11-08.md).
LUCI_RUNTIME_DIR ?=

CONTAINER := $(DOCKER) run --rm --platform linux/arm64 -v "$(CURDIR)":/src -w /src \
	-v cowplay-cargo-registry:/usr/local/cargo/registry \
	-e CARGO_TARGET_DIR=/src/$(TARGET_DIR) $(BUILD_IMAGE) sh -ec

.DEFAULT_GOAL := help
.PHONY: help init deps check-version web build apk luci-apk test test-rust test-web test-luci \
	test-package test-apk deploy check-device

help:
	@echo 'Package version: $(VERSION)'
	@echo '  make init          check tools, pull the build image, install locked dependencies'
	@echo '  make deps          npm ci (web/package-lock.json) and cargo fetch --locked'
	@echo '  make build         web bundle, then cowplay + luci-app-cowplay APKs in $(APK_DIR)/'
	@echo '  make luci-apk      only luci-app-cowplay APK'
	@echo '  make test          Rust, synthetic browser and package-script tests (local)'
	@echo '  make test-luci     LuCI page against official runtime; needs LUCI_RUNTIME_DIR=...'
	@echo '  make test-apk      install/remove both APKs in a disposable OpenWrt root (container)'
	@echo '  make deploy        upload and upgrade both APKs on DEVICE=$(DEVICE)'
	@echo '  make check-device  read-only: is the CowPlay LuCI view served by DEVICE_HTTP?'

init:
	@for tool in $(DOCKER) node npm cargo python3; do \
		command -v $$tool >/dev/null || { echo "missing tool: $$tool" >&2; exit 1; }; done
	@$(DOCKER) info >/dev/null || { echo 'container runtime is not running' >&2; exit 1; }
	$(DOCKER) image inspect $(BUILD_IMAGE) >/dev/null 2>&1 || $(DOCKER) pull --platform linux/arm64 $(BUILD_IMAGE)
	mkdir -p target
	$(MAKE) deps

deps:
	npm ci --prefix web
	cargo fetch --locked

# Package version (build-apk.sh and the package tests) must follow Cargo.toml.
check-version:
	@version='$(VERSION)'; revision=$${version#"$(CRATE_VERSION)-r"}; \
		case "$$revision" in ''|*[!0-9]*) echo 'invalid package revision: $(VERSION)' >&2; exit 1;; esac
	@for f in scripts/test-apk.sh scripts/test-installed-package.sh; do grep -q "cowplay=$(VERSION)'" $$f || \
		{ echo "$$f does not expect cowplay=$(VERSION)" >&2; exit 1; }; done

web:
	npm run build --prefix web

build: web apk

# prepare-bluez.sh refuses an existing directory; an earlier runtime is reused as is.
$(BLUEZ_RUNTIME)/SHA256SUMS:
	$(CONTAINER) 'apk add --no-cache curl >/dev/null; sh scripts/prepare-bluez.sh $(BLUEZ_RUNTIME)'

apk: check-version $(BLUEZ_RUNTIME)/SHA256SUMS
	mkdir -p $(APK_DIR)
	$(CONTAINER) 'apk add --no-cache musl-dev curl >/dev/null; \
		cargo build --locked --release; \
		sh scripts/build-apk.sh $(TARGET_DIR)/release/cowplay $(CORE_APK) $(BLUEZ_RUNTIME); \
		sh scripts/build-luci-apk.sh $(LUCI_APK)'
	@cd $(APK_DIR) && shasum -a 256 $(notdir $(CORE_APK)) $(notdir $(LUCI_APK))

luci-apk: check-version
	mkdir -p $(APK_DIR)
	$(CONTAINER) 'sh scripts/build-luci-apk.sh $(LUCI_APK)'
	@cd $(APK_DIR) && shasum -a 256 $(notdir $(LUCI_APK))

linux-tar: check-version
	python3 scripts/build-linux-tar.py "$(LINUX_BINARY)" "$(LINUX_ARCH)" "$(LINUX_TAR)"

test: test-rust test-web test-package

test-rust:
	cargo test --workspace --locked

test-web: web
	node scripts/test-browser-startup.mjs
	node scripts/test-audio-web.mjs
	node scripts/test-mic-web.mjs
	node scripts/test-frame-progress.mjs
	node scripts/test-viewer-ping.mjs
	node scripts/test-pairing-web.mjs
	node scripts/test-car-pairing-web.mjs
	node scripts/test-connection-diagnostics-web.mjs
	node scripts/test-confirmation-web.mjs
	node scripts/test-phone-operation-ux-web.mjs
	node scripts/test-phone-pairing-cancel-web.mjs
	node scripts/test-car-operation-ux-web.mjs
	node scripts/test-playport-alignment-web.mjs
	node scripts/test-linux-web.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-tesla-management-web.mjs
	node scripts/test-viewer-web.mjs

test-luci: web
	@[ -n "$(LUCI_RUNTIME_DIR)" ] || { echo 'set LUCI_RUNTIME_DIR to the official LuCI runtime' >&2; exit 1; }
	python3 scripts/test-private-key-download.py
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-luci-sampling-lifecycle.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-luci-download.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-luci-diagnostics-details.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-luci-web.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" LUCI_DEVICE_APPEND=1 node scripts/test-luci-web.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-certificates-web.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" LUCI_DEVICE_APPEND=1 node scripts/test-certificates-web.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-network-web.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" LUCI_DEVICE_APPEND=1 node scripts/test-network-web.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-luci-operation-ux.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" LUCI_DEVICE_APPEND=1 node scripts/test-luci-operation-ux.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-tesla-management-web.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-updater-luci.mjs
	LUCI_RUNTIME_DIR="$(LUCI_RUNTIME_DIR)" node scripts/test-tesla-navigation-web.mjs

test-package:
	mkdir -p target
	python3 scripts/test-package-lifecycle.py
	python3 scripts/test-deploy-autostart.py

# Each run uses a new root so earlier evidence stays untouched.
test-apk:
	@[ -f $(CORE_APK) ] && [ -f $(LUCI_APK) ] || { echo 'run make build first' >&2; exit 1; }
	$(CONTAINER) 'apk add --no-cache curl >/dev/null; \
		sh scripts/test-luci-apk.sh $(CORE_APK) $(LUCI_APK) target/apk-luci-test-$$(date +%Y%m%d-%H%M%S)'

deploy:
	@[ -f $(CORE_APK) ] && [ -f $(LUCI_APK) ] || { echo 'run make build first' >&2; exit 1; }
	sh scripts/deploy-apk.sh $(DEVICE) $(VERSION) $(CORE_APK) $(LUCI_APK)

check-device:
	@code=$$(curl -s -o /dev/null -m 5 -w '%{http_code}' $(DEVICE_HTTP)/luci-static/resources/view/cowplay/main.js); \
		echo "LuCI CowPlay view: HTTP $$code"; [ "$$code" = 200 ]
