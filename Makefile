# CowPlay developer entry points. Run from the repository root; `make help` lists targets.
# AArch64 builds and APK checks run in an arm64 Alpine container (Docker or OrbStack).
# deploy changes the F50: run it only when the current task authorizes installing there.

VERSION := $(shell cat packaging/VERSION)
CRATE_VERSION := $(shell sed -n '/^\[package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
DOCKER ?= docker
BUILD_IMAGE ?= rust:1.99-alpine@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383
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
# Needed by LuCI browser tests; prepare with scripts/prepare-luci-test-runtime.py.
LUCI_RUNTIME_DIR ?=
RELEASE_DIR ?= target/release-assets
RELEASE_NOTES ?= target/release-notes.md
RELEASE_APK := $(RUNNER_TEMP)/apk

CONTAINER := $(DOCKER) run --rm --platform linux/arm64 -v "$(CURDIR)":/src -w /src \
	-v cowplay-cargo-registry:/usr/local/cargo/registry \
	-e CARGO_TARGET_DIR=/src/$(TARGET_DIR) $(BUILD_IMAGE) sh -ec

.DEFAULT_GOAL := help
.PHONY: help init deps check-version web build apk luci-apk test test-rust test-web test-luci \
	test-package test-apk deploy check-device check-release ci-web ci-browser ci-build \
	bluez-native apk-native test-apk-native release-tool release-assets release-test release-sign release-publish

help:
	@echo 'Package version: $(VERSION)'
	@echo '  make init          check tools, pull the build image, install locked dependencies'
	@echo '  make deps          npm ci (web/package-lock.json) and cargo fetch --locked'
	@echo '  make build         web bundle, then cowplay + luci-app-cowplay APKs in $(APK_DIR)/'
	@echo '  make luci-apk      only luci-app-cowplay APK'
	@echo '  make test          Rust, synthetic browser and package-script tests (local)'
	@echo '  make test-luci     LuCI page against official runtime; needs LUCI_RUNTIME_DIR=...'
	@echo '  make test-apk      install/remove both APKs in a disposable OpenWrt root (container)'
	@echo '  make check-release check versions, compatibility policy and release tag'
	@echo '  make ci-web        install locked frontend dependencies, build and run fast checks'
	@echo '  make ci-browser    run release browser checks with CHROME_BIN and LUCI_RUNTIME_DIR'
	@echo '  make ci-build      build/test in the native Rust Alpine Action container'
	@echo '  make release-test  verify complete assets with a disposable signing key (RUNNER_TEMP required)'
	@echo '  make release-sign  sign complete assets with the configured production key'
	@echo '  make release-publish upload, verify and publish the signed assets on GitHub'
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

check-release: check-version
	@python3 -c 'import ast, json, re; from pathlib import Path; \
		p = json.loads(Path("packaging/release-policy.json").read_text()); \
		assert p["repository"] == "helloworlde/cowplay"; \
		assert p["channel"] in ["stable", "dev"]; \
		assert p["openwrt_releases"] and isinstance(p["upgrade_from"], list); \
		assert all(re.fullmatch(r"\d+\.\d+\.\d+", v) for v in p["openwrt_releases"]); \
		assert all(re.fullmatch(r"\d+\.\d+\.\d+-r\d+", v) for v in p["upgrade_from"]); \
		front = Path("web/src/build.ts").read_text(); back = Path("crates/server/src/build_info.rs").read_text(); \
		assert all(ast.literal_eval(re.search(name + r"[^=]*=\s*([^;]+)", front)[1]) == \
			ast.literal_eval(re.search(name + r"[^=]*=\s*([^;]+)", back)[1]) for name in ["BUILD_ID", "PROTOCOL"])'
	@case "$${GITHUB_REF:-}" in refs/tags/*) \
		git merge-base --is-ancestor "$$GITHUB_SHA" origin/main && \
		test "$$GITHUB_REF_NAME" = 'v$(patsubst %-r0,%,$(VERSION))';; esac

ci-web: SHELL := /bin/bash
ci-web:
	npm ci --prefix web
	npm audit --prefix web --audit-level=high
	$(MAKE) web
	node scripts/test-frame-progress.mjs
	node scripts/test-viewer-ping.mjs
	node scripts/test-video-resolution.mjs
	set -o pipefail; find luci-app-cowplay/htdocs -name '*.js' -print0 | xargs -0 -n 1 node --check

ci-browser:
	timeout 15s "$(CHROME_BIN)" --version
	python3 scripts/prepare-luci-test-runtime.py "$(LUCI_RUNTIME_DIR)"
	node scripts/test-access-setup-web.mjs
	node scripts/test-car-access-web.mjs
	node scripts/test-tesla-navigation-web.mjs
	node scripts/test-updater-luci.mjs
	node scripts/test-luci-operation-ux.mjs
	node scripts/test-tesla-management-web.mjs
	node scripts/test-viewer-web.mjs

ci-build:
	apk add --no-cache musl-dev openssl python3 coreutils util-linux curl
	cargo build --locked --release
	$(MAKE) linux-tar LINUX_BINARY="$(CARGO_TARGET_DIR)/release/cowplay"
	$(MAKE) test-rust
	python3 scripts/test-linux-install.py
	python3 scripts/test-linux-download.py
	@if [ '$(LINUX_ARCH)' = arm64 ]; then \
		$(MAKE) apk-native TARGET_DIR=target/linux && \
		$(MAKE) test-apk-native TARGET_DIR=target/linux && \
		python3 scripts/test-package-lifecycle.py && \
		python3 scripts/test-sysupgrade-restore.py && \
		python3 scripts/test-release-artifacts.py; \
	fi

web:
	npm run build --prefix web

build: web apk

# prepare-bluez.sh refuses an existing directory; an earlier runtime is reused as is.
$(BLUEZ_RUNTIME)/SHA256SUMS:
	$(CONTAINER) 'apk add --no-cache curl >/dev/null; sh scripts/prepare-bluez.sh $(BLUEZ_RUNTIME)'

apk: check-version $(BLUEZ_RUNTIME)/SHA256SUMS
	$(CONTAINER) 'apk add --no-cache make musl-dev curl >/dev/null; \
		make apk-native TARGET_DIR=$(TARGET_DIR) APK_DIR=$(APK_DIR) BLUEZ_RUNTIME=$(BLUEZ_RUNTIME)'

bluez-native:
	@test -f $(BLUEZ_RUNTIME)/SHA256SUMS || sh scripts/prepare-bluez.sh $(BLUEZ_RUNTIME)

apk-native: check-version bluez-native
	mkdir -p $(APK_DIR)
	cargo build --locked --release
	sh scripts/build-apk.sh $(TARGET_DIR)/release/cowplay $(CORE_APK) $(BLUEZ_RUNTIME)
	sh scripts/build-luci-apk.sh $(LUCI_APK)
	@cd $(APK_DIR) && sha256sum $(notdir $(CORE_APK)) $(notdir $(LUCI_APK))

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
	python3 scripts/test-private-key-download.py
	python3 scripts/test-package-lifecycle.py
	python3 scripts/test-deploy-autostart.py

# Each run uses a new root so earlier evidence stays untouched.
test-apk:
	@[ -f $(CORE_APK) ] && [ -f $(LUCI_APK) ] || { echo 'run make build first' >&2; exit 1; }
	$(CONTAINER) 'apk add --no-cache make curl >/dev/null; \
		make test-apk-native APK_DIR=$(APK_DIR)'

test-apk-native:
	@[ -f $(CORE_APK) ] && [ -f $(LUCI_APK) ] || { echo 'run make build first' >&2; exit 1; }
	sh scripts/test-luci-apk.sh $(CORE_APK) $(LUCI_APK) target/apk-luci-test-$$(date +%Y%m%d-%H%M%S)

release-tool:
	@test -n "$(RUNNER_TEMP)" && test -d "$(RUNNER_TEMP)"
	$(DOCKER) pull $(BUILD_IMAGE)
	@printf '%s\n' '#!/bin/sh' \
		'exec $(DOCKER) run --rm --network none --user "$$(id -u):$$(id -g)" \' \
		'  -v "$$GITHUB_WORKSPACE:$$GITHUB_WORKSPACE" -v "$$RUNNER_TEMP:$$RUNNER_TEMP" \' \
		'  -w "$$GITHUB_WORKSPACE" $(BUILD_IMAGE) apk "$$@"' > "$(RELEASE_APK)"
	@chmod 700 "$(RELEASE_APK)"

release-assets:
	python3 scripts/prepare-github-release.py --policy packaging/release-policy.json \
		--core "$(CORE_APK)" --luci "$(LUCI_APK)" \
		--linux-x86_64 "target/linux-dist/cowplay-$(VERSION)-linux-x86_64.tar.gz" \
		--linux-arm64 "target/linux-dist/cowplay-$(VERSION)-linux-arm64.tar.gz" \
		--sign-key "$(RELEASE_KEY)" --apk "$(RELEASE_APK)" --output "$(RELEASE_DIR)" --notes "$(RELEASE_NOTES)"

release-test: release-tool
	@set -eu; export TMPDIR="$(RUNNER_TEMP)"; umask 077; key=$$(mktemp "$(RUNNER_TEMP)/test-release.XXXXXX"); \
		trap 'rm -f "$$key"' EXIT; \
		openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$$key"; \
		$(MAKE) release-assets RELEASE_KEY="$$key" RELEASE_DIR=target/test-release-assets RELEASE_NOTES=target/test-release-notes.md; \
		cd target/test-release-assets; \
		openssl dgst -sha256 -verify release-key.pem -signature SHA256SUMS.sig SHA256SUMS; \
		sha256sum -c SHA256SUMS

release-sign: SHELL := /bin/bash
release-sign: release-tool
	@set -euo pipefail; export TMPDIR="$(RUNNER_TEMP)"; umask 077; key=$$(mktemp "$(RUNNER_TEMP)/cowplay-release.XXXXXX"); \
		trap 'rm -f "$$key"' EXIT; \
		test -n "$${RELEASE_SIGNING_KEY:-}"; test -n "$${EXPECTED_PUBLIC_KEY_SHA256:-}"; \
		printf '%s\n' "$$RELEASE_SIGNING_KEY" > "$$key"; unset RELEASE_SIGNING_KEY; \
		actual=$$(openssl pkey -in "$$key" -pubout | sha256sum | cut -d ' ' -f 1); \
		test "$$actual" = "$$EXPECTED_PUBLIC_KEY_SHA256"; \
		test "$$actual" = "$$(sha256sum packaging/release-key.pem | cut -d ' ' -f 1)"; \
		$(MAKE) release-assets RELEASE_KEY="$$key"

release-publish:
	@set -eu; tag="$${GITHUB_REF_NAME:?}"; export GH_REPO="$${GITHUB_REPOSITORY:?}"; \
		channel=$$(python3 -c 'import json; print(json.load(open("packaging/release-policy.json"))["channel"])'); \
		case "$$channel" in \
			dev) prerelease=true; latest=false;; \
			stable) prerelease=false; latest=true;; \
			*) echo 'Invalid release channel' >&2; exit 1;; \
		esac; \
		if draft=$$(gh release view "$$tag" --json isDraft --jq .isDraft); then \
			test "$$draft" = true || { echo 'Published releases cannot be overwritten' >&2; exit 1; }; \
		else \
			gh release create "$$tag" --verify-tag --draft --title "CowPlay $$tag" \
				--prerelease="$$prerelease" --latest=false --notes-file "$(RELEASE_NOTES)"; \
		fi; \
		gh release upload "$$tag" "$(RELEASE_DIR)"/* --clobber; \
		downloaded=$$(mktemp -d "$(RUNNER_TEMP)/downloaded-release.XXXXXX"); \
		gh release download "$$tag" --dir "$$downloaded"; \
		diff --recursive "$(RELEASE_DIR)" "$$downloaded"; \
		gh release edit "$$tag" --notes-file "$(RELEASE_NOTES)" \
			--draft=false --prerelease="$$prerelease" --latest="$$latest"

deploy:
	@[ -f $(CORE_APK) ] && [ -f $(LUCI_APK) ] || { echo 'run make build first' >&2; exit 1; }
	sh scripts/deploy-apk.sh $(DEVICE) $(VERSION) $(CORE_APK) $(LUCI_APK)

check-device:
	@code=$$(curl -s -o /dev/null -m 5 -w '%{http_code}' $(DEVICE_HTTP)/luci-static/resources/view/cowplay/main.js); \
		echo "LuCI CowPlay view: HTTP $$code"; [ "$$code" = 200 ]
